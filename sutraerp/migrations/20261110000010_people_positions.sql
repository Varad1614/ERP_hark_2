-- Batch 7 slice 7A — institutional positions, position assignments (occupancy) and the governed
-- engagement change requests (activation / modification / transfer; L3-E-001…003, 011…013).
--
-- Owned by 7A (docs/batch7-contracts.md §3.1). Org units are Domain B ids (plain uuid, validated
-- through InstitutionReferencePort). Engagements / staff records are foundation tables (FK allowed).

-- ------------------------------------------------------------------ positions (L2-E02)
CREATE TABLE people.position (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    org_unit_id               uuid,
    code                      text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_.-]{0,31}$'),
    title                     text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 160),
    staff_category            text         CHECK (staff_category IS NULL OR staff_category IN ('teaching', 'non_teaching', 'administrative')),
    grade                     text         CHECK (grade IS NULL OR grade ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$'),
    -- More than one primary occupant at a time (shared / pooled positions).
    allows_multiple           boolean      NOT NULL DEFAULT false,
    status                    text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'frozen', 'retired', 'archived')),
    status_reason             text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX position_code_per_institution ON people.position (tenant_id, institution_id, code);
CREATE INDEX position_institution_idx ON people.position (tenant_id, institution_id, status);
CREATE TRIGGER position_no_delete BEFORE DELETE ON people.position
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ assignments (occupancy)
CREATE TABLE people.position_assignment (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    position_id               uuid         NOT NULL REFERENCES people.position (id),
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    is_primary                boolean      NOT NULL DEFAULT true,
    start_date                date         NOT NULL,
    end_date                  date,
    -- Reassignment (L3-E-012): the assignment this one replaces (superseded when this activates).
    supersedes_id             uuid         REFERENCES people.position_assignment (id),
    -- Completion / termination outcome (L3-E-013).
    end_outcome               text         CHECK (end_outcome IS NULL OR end_outcome IN ('completed', 'terminated', 'superseded', 'engagement_closed')),
    reason                    text,
    status                    text         NOT NULL DEFAULT 'pending_approval'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'scheduled', 'active', 'suspended', 'closed', 'superseded', 'archived')),
    status_reason             text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= start_date)
);
-- One active primary occupancy per engagement.
CREATE UNIQUE INDEX position_assignment_one_primary_per_engagement
    ON people.position_assignment (engagement_id) WHERE is_primary AND status = 'active';
CREATE INDEX position_assignment_position_idx ON people.position_assignment (tenant_id, position_id, status);
CREATE INDEX position_assignment_engagement_idx ON people.position_assignment (tenant_id, engagement_id);
CREATE TRIGGER position_assignment_no_delete BEFORE DELETE ON people.position_assignment
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ engagement change requests
-- Governed engagement decisions (ENGAGEMENT_ACTIVATION, ENGAGEMENT_CHANGE): one open request per
-- engagement; the proposal is applied to `people.staff_engagement` on approval.
CREATE TABLE people.engagement_change_request (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    kind                      text         NOT NULL CHECK (kind IN ('activation', 'modification', 'transfer')),
    -- camelCase proposal (modification / transfer) as submitted.
    proposal                  jsonb        NOT NULL DEFAULT '{}'::jsonb,
    effective_date            date,
    reason                    text,
    status                    text         NOT NULL DEFAULT 'pending_approval'
        CHECK (status IN ('pending_approval', 'approved', 'rejected', 'applied', 'cancelled')),
    decision_reason           text,
    decided_by                text,
    decided_at                timestamptz,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX engagement_change_request_one_open
    ON people.engagement_change_request (engagement_id) WHERE status IN ('pending_approval', 'approved');
CREATE INDEX engagement_change_request_engagement_idx
    ON people.engagement_change_request (tenant_id, engagement_id, created_at);
CREATE TRIGGER engagement_change_request_no_delete BEFORE DELETE ON people.engagement_change_request
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
