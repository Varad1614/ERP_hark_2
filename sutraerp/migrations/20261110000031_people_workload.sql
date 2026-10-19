-- Batch 7 slice 7B — faculty availability + workload plan (L3-E-030). Owned by 7B.
-- Teaching load comes from Domain D through `FacultyWorkloadPort` (no cross-schema SQL); the plan
-- stores the snapshot it was approved against. Hours are NUMERIC(5,2).

CREATE TABLE people.workload_plan (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    staff_id              uuid          NOT NULL REFERENCES people.staff_member (id),
    engagement_id         uuid          NOT NULL REFERENCES people.staff_engagement (id),
    -- Domain D academic term (plain id, no FK) and a label for the planning period.
    academic_term_id      uuid,
    term_label            text          NOT NULL CHECK (length(btrim(term_label)) BETWEEN 1 AND 120),
    availability          text          NOT NULL CHECK (availability IN ('available', 'partially_available', 'unavailable')),
    available_from        date,
    available_to          date,
    planned_weekly_hours  numeric(5,2)  NOT NULL CHECK (planned_weekly_hours >= 0),
    -- Plan-specific maximum (else the Domain D faculty assignment policy maximum applies).
    max_weekly_hours      numeric(5,2)  CHECK (max_weekly_hours IS NULL OR max_weekly_hours > 0),
    -- Snapshot at submission.
    teaching_weekly_hours numeric(5,2),
    effective_max_hours   numeric(5,2),
    over_allocated        boolean       NOT NULL DEFAULT false,
    exception_reason      text,
    notes                 text,
    status                text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'approved', 'active', 'superseded')),
    status_reason         text,
    process_instance_id   text,
    created_by            text,
    decided_by            text,
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    updated_at            timestamptz   NOT NULL DEFAULT now(),
    CHECK (available_to IS NULL OR available_from IS NULL OR available_to >= available_from)
);
CREATE INDEX workload_plan_staff_idx ON people.workload_plan (tenant_id, staff_id, status);
-- One active plan per staff member and planning period.
CREATE UNIQUE INDEX workload_plan_one_active
    ON people.workload_plan (staff_id, COALESCE(academic_term_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE status = 'active';
