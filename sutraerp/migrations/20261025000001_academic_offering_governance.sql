-- Batch 4 slice 4A (docs/batch4-contracts.md §3.3): course offering governance tables.
--
-- The offering, section, faculty assignment and section enrolment rows themselves live in the
-- foundation tables (20261025000000). This migration adds 4A's own aggregates around them:
-- governed offering change requests (revision / replacement / rollback / cancellation /
-- unfreeze, L3-D-004/009/013), merge/split reorganisations (L3-D-011), templates and their
-- generation provenance (L3-D-012), change simulations (L3-D-016), capacity history (L3-D-005),
-- quotas (L3-D-015), enrolment windows (L3-D-014), the waitlist (L3-D-007), enrolment
-- restrictions incl. cross-campus / cross-institution permits (L3-D-006/008), faculty assignment
-- governance configuration (L3-D-019) and operational changes (L3-D-018).
--
-- Domain B/C references are plain uuids (no FK across schemas); FKs only to academic tables.

-- ---------------------------------------------------------------- governed offering requests
CREATE TABLE academic.offering_revision (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    kind                text        NOT NULL CHECK (kind IN (
        'revision', 'replacement', 'rollback', 'cancellation', 'unfreeze')),
    -- Proposed attribute values (camelCase API names) for revision / replacement.
    proposed            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- Offering attributes before / after the change was applied (immutable history).
    baseline            jsonb,
    snapshot_after      jsonb,
    -- Rollback target (an applied revision of the same offering).
    target_revision_id  uuid        REFERENCES academic.offering_revision (id),
    -- Lifecycle position the request restores (unfreeze, lifecycle rollback).
    restore_status      text,
    status              text        NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    reason              text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    decision_reason     text,
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    applied_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_revision_offering_idx ON academic.offering_revision (offering_id, created_at);
-- At most one open request per offering and kind.
CREATE UNIQUE INDEX offering_revision_one_open
    ON academic.offering_revision (offering_id, kind)
    WHERE status IN ('requested', 'validated', 'approved');

-- ---------------------------------------------------------------- merge / split
CREATE TABLE academic.offering_reorganisation (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    -- Merge: the surviving target; split: the source being split.
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    kind                text        NOT NULL CHECK (kind IN ('merge', 'split')),
    -- Merge: the offerings absorbed into `offering_id`.
    source_offering_ids uuid[]      NOT NULL DEFAULT '{}',
    -- Split: requested parts `[{offeringCode, sectionIds[]}]`.
    plan                jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- Offerings created by a split.
    result_offering_ids uuid[]      NOT NULL DEFAULT '{}',
    status              text        NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    reason              text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    decision_reason     text,
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    applied_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_reorganisation_offering_idx ON academic.offering_reorganisation (offering_id);

-- ---------------------------------------------------------------- templates
CREATE TABLE academic.offering_template (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    name                text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    delivery_mode       text        NOT NULL DEFAULT 'in_person'
        CHECK (delivery_mode IN ('in_person', 'online', 'hybrid')),
    capacity            integer     CHECK (capacity IS NULL OR capacity >= 0),
    section_codes       text[]      NOT NULL DEFAULT ARRAY['A'],
    section_capacity    integer     CHECK (section_capacity IS NULL OR section_capacity >= 0),
    campus_id           uuid,
    -- Generated offering codes: `{code}-{term}` where {code} = course code or slot code.
    code_suffix         text,
    status              text        NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'retired')),
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_template_institution_idx ON academic.offering_template (tenant_id, institution_id);

-- Provenance + per-component outcome of each generation run (L4-D-012-BH-005/008).
CREATE TABLE academic.offering_template_generation (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    template_id             uuid        NOT NULL REFERENCES academic.offering_template (id),
    run_id                  uuid        NOT NULL,
    academic_term_id        uuid        NOT NULL,
    curriculum_component_id uuid        NOT NULL,
    offering_id             uuid        REFERENCES academic.course_offering (id),
    outcome                 text        NOT NULL CHECK (outcome IN ('generated', 'failed')),
    detail                  text,
    created_at              timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_template_generation_run_idx ON academic.offering_template_generation (template_id, run_id);
CREATE UNIQUE INDEX offering_template_generation_offering ON academic.offering_template_generation (offering_id)
    WHERE offering_id IS NOT NULL;
CREATE TRIGGER offering_template_generation_immutable
    BEFORE UPDATE OR DELETE ON academic.offering_template_generation
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- ---------------------------------------------------------------- simulations
CREATE TABLE academic.offering_change_simulation (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    change_kind         text        NOT NULL CHECK (change_kind IN (
        'capacity', 'cancellation', 'suspension', 'section_cancellation', 'merge', 'faculty_change')),
    proposal            jsonb       NOT NULL DEFAULT '{}'::jsonb,
    criteria            text,
    report              jsonb       NOT NULL,
    status              text        NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    -- Governance decision on the evidence: proceed | revise | reject (never executes the change).
    decision            text        CHECK (decision IN ('proceed', 'revise', 'reject')),
    decision_reason     text,
    decided_by          text,
    decided_at          timestamptz,
    process_instance_id text,
    requested_by        text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_change_simulation_offering_idx ON academic.offering_change_simulation (offering_id, created_at);

-- ---------------------------------------------------------------- capacity history
CREATE TABLE academic.offering_capacity_change (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL,
    offering_id     uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id      uuid        REFERENCES academic.offering_section (id),
    from_capacity   integer,
    to_capacity     integer     CHECK (to_capacity IS NULL OR to_capacity >= 0),
    seated_at_change integer    NOT NULL DEFAULT 0,
    reason          text        NOT NULL,
    actor           text,
    effective_on    date        NOT NULL,
    recorded_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX offering_capacity_change_offering_idx ON academic.offering_capacity_change (offering_id, recorded_at);
CREATE TRIGGER offering_capacity_change_immutable
    BEFORE UPDATE OR DELETE ON academic.offering_capacity_change
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- ---------------------------------------------------------------- quotas
CREATE TABLE academic.offering_quota (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL,
    offering_id     uuid        NOT NULL REFERENCES academic.course_offering (id),
    category        text        NOT NULL CHECK (category ~ '^[a-z][a-z0-9_]{0,39}$'),
    seats           integer     NOT NULL CHECK (seats >= 0),
    effective_from  date,
    effective_to    date,
    rationale       text        NOT NULL,
    status          text        NOT NULL DEFAULT 'effective' CHECK (status IN ('effective', 'superseded', 'retired')),
    superseded_by   uuid        REFERENCES academic.offering_quota (id),
    version         bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT offering_quota_dates CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to >= effective_from)
);
CREATE UNIQUE INDEX offering_quota_one_effective
    ON academic.offering_quota (offering_id, category) WHERE status = 'effective';

-- ---------------------------------------------------------------- enrolment windows
CREATE TABLE academic.offering_enrolment_window (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL,
    offering_id     uuid        NOT NULL REFERENCES academic.course_offering (id),
    kind            text        NOT NULL DEFAULT 'regular' CHECK (kind IN ('regular', 'add_drop', 'late')),
    opens_on        date        NOT NULL,
    closes_on       date        NOT NULL,
    status          text        NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'open', 'suspended', 'closed', 'closed_permanently')),
    status_reason   text,
    version         bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT offering_enrolment_window_dates CHECK (closes_on >= opens_on)
);
CREATE INDEX offering_enrolment_window_offering_idx ON academic.offering_enrolment_window (offering_id);

-- ---------------------------------------------------------------- waitlist
CREATE TABLE academic.offering_waitlist_entry (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    -- Preferred section (NULL = any active section).
    section_id          uuid        REFERENCES academic.offering_section (id),
    student_id          uuid        NOT NULL,
    student_context_id  uuid        NOT NULL,
    student_number      text        NOT NULL,
    display_name        text        NOT NULL,
    subject_ref         text,
    position            integer     NOT NULL CHECK (position >= 1),
    status              text        NOT NULL DEFAULT 'waiting'
        CHECK (status IN ('waiting', 'offered', 'accepted', 'expired', 'removed')),
    status_reason       text,
    offered_section_id  uuid        REFERENCES academic.offering_section (id),
    offered_at          timestamptz,
    offer_expires_on    date,
    enrolment_id        uuid        REFERENCES academic.section_enrolment (id),
    resolved_at         timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX offering_waitlist_one_live
    ON academic.offering_waitlist_entry (offering_id, student_id) WHERE status IN ('waiting', 'offered');
CREATE INDEX offering_waitlist_queue_idx ON academic.offering_waitlist_entry (offering_id, status, position);

-- ---------------------------------------------------------------- eligibility / restrictions
CREATE TABLE academic.enrolment_restriction (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    -- programme: only students of the listed programmes; min_term: current term >= min_term_no;
    -- cross_campus: students of the listed campuses may enrol in a campus-bound offering;
    -- cross_institution: students of the listed home institutions may enrol (both approvals).
    kind                text        NOT NULL CHECK (kind IN ('programme', 'min_term', 'cross_campus', 'cross_institution')),
    ref_ids             uuid[]      NOT NULL DEFAULT '{}',
    min_term_no         integer     CHECK (min_term_no IS NULL OR min_term_no >= 1),
    home_approval_ref   text,
    host_approval_ref   text,
    seat_limit          integer     CHECK (seat_limit IS NULL OR seat_limit >= 0),
    effective_from      date,
    effective_to        date,
    reason              text        NOT NULL,
    status              text        NOT NULL DEFAULT 'effective' CHECK (status IN ('effective', 'retired')),
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT enrolment_restriction_dates CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to >= effective_from),
    CONSTRAINT enrolment_restriction_cross_institution CHECK (
        kind <> 'cross_institution' OR (home_approval_ref IS NOT NULL AND host_approval_ref IS NOT NULL))
);
CREATE INDEX enrolment_restriction_offering_idx ON academic.enrolment_restriction (offering_id, status);

-- ---------------------------------------------------------------- faculty assignment governance
CREATE TABLE academic.faculty_assignment_policy (
    id                          uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                   uuid        NOT NULL,
    institution_id              uuid        NOT NULL,
    name                        text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    version_no                  integer     NOT NULL CHECK (version_no >= 1),
    max_weekly_hours            numeric(5,2) CHECK (max_weekly_hours IS NULL OR max_weekly_hours > 0),
    allowed_roles               text[]      NOT NULL DEFAULT '{}',
    require_primary_instructor  boolean     NOT NULL DEFAULT true,
    rationale                   text        NOT NULL,
    review_outcome              text        CHECK (review_outcome IN ('compatible', 'incompatible')),
    review_notes                text,
    status                      text        NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'in_review', 'effective', 'superseded', 'retired')),
    status_reason               text,
    process_instance_id         text,
    superseded_by               uuid        REFERENCES academic.faculty_assignment_policy (id),
    effective_at                timestamptz,
    version                     bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    updated_at                  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX faculty_assignment_policy_one_effective
    ON academic.faculty_assignment_policy (tenant_id, institution_id) WHERE status = 'effective';
CREATE UNIQUE INDEX faculty_assignment_policy_version_no
    ON academic.faculty_assignment_policy (tenant_id, institution_id, version_no);

CREATE TABLE academic.faculty_assignment_change (
    id                          uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                   uuid        NOT NULL,
    institution_id              uuid        NOT NULL,
    assignment_id               uuid        NOT NULL REFERENCES academic.faculty_assignment (id),
    offering_id                 uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id                  uuid        NOT NULL REFERENCES academic.offering_section (id),
    change_kind                 text        NOT NULL CHECK (change_kind IN ('role', 'hours', 'period', 'reassignment')),
    proposed                    jsonb       NOT NULL,
    baseline                    jsonb       NOT NULL,
    status                      text        NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    reason                      text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    decision_reason             text,
    process_instance_id         text,
    replacement_assignment_id   uuid        REFERENCES academic.faculty_assignment (id),
    requested_by                text,
    decided_by                  text,
    applied_at                  timestamptz,
    version                     bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    updated_at                  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX faculty_assignment_change_assignment_idx ON academic.faculty_assignment_change (assignment_id, created_at);
CREATE UNIQUE INDEX faculty_assignment_change_one_open
    ON academic.faculty_assignment_change (assignment_id) WHERE status IN ('requested', 'validated', 'approved');
