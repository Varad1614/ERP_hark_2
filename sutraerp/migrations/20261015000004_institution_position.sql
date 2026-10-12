-- Batch 2 slice 2A: effective-dated position definitions (L3-B-013..015). A definition never
-- creates an occupant or a PositionAssignment (Section E). Each revision is immutable once the
-- definition is active; a change is a new revision effective from a later business-local date
-- (institution timezone) and closes its predecessor the day before.
CREATE TABLE institution.position_definition (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL REFERENCES institution.institution(id),
    code           text        NOT NULL,
    status         text        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'retired', 'archived')),
    status_reason  text,
    latest_revision integer    NOT NULL DEFAULT 1 CHECK (latest_revision >= 1),
    retired_on     date,
    version        bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    -- Codes are never reused, archived definitions included (L4-B-015-BH-006).
    CONSTRAINT position_definition_code_unique UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE institution.position_definition_revision (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL REFERENCES institution.institution(id),
    position_id    uuid        NOT NULL REFERENCES institution.position_definition(id),
    revision       integer     NOT NULL CHECK (revision >= 1),
    org_unit_id    uuid        NOT NULL REFERENCES institution.org_unit(id),
    title          text        NOT NULL,
    grade          text,
    description    text,
    headcount      integer     NOT NULL CHECK (headcount >= 1),
    effective_from date        NOT NULL,
    effective_to   date,
    reason         text,
    recorded_by    text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT position_definition_revision_unique UNIQUE (position_id, revision),
    CONSTRAINT position_definition_revision_dates CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE INDEX position_definition_revision_unit_idx
    ON institution.position_definition_revision (org_unit_id);
