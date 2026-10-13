-- Batch 4 foundation (contract doc docs/batch4-contracts.md §3).
--
-- The shared Academic delivery aggregates every Batch 4 slice references: Course Offering
-- (L2-D01), its teaching Sections, Faculty Academic Assignments (L2-D02) and the Section
-- Enrolment roster, plus their immutable status history. Written by slice 4A (status changes
-- only through `sutra_academic::shared::lifecycle`); read by 4B/4C through
-- `sutra_contracts::academic::ports::OfferingDirectory` (`sutra_academic::PgOfferingDirectory`).
--
-- Other domains' data is referenced by plain `uuid` columns, never joined or FK'd
-- (ADR-L5-0036 §3.2): Domain B (institution, campus, org unit, curriculum version/component,
-- academic year/term) through `InstitutionReferencePort`; Domain C (student, student context,
-- registration) through `StudentDirectory` / `RegistrationReader`; staff (Domain E) by login
-- subject through `StaffDirectory`.
--
-- Reserved migration ranges (one file per change, never edit an applied file):
--   4A  20261025000001 .. 20261025000009
--   4B  20261025000010 .. 20261025000029
--   4C  20261025000030 .. 20261025000039

-- Append-only guard every slice reuses for histories, ledgers and attendance audit trails.
CREATE FUNCTION academic.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of academic.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- Course Offering: one curriculum component offered in one academic term.
CREATE TABLE academic.course_offering (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL,
    -- Domain B campus delivering the offering (NULL = institution-wide).
    campus_id               uuid,
    -- Owning department (Domain B org unit), when known.
    org_unit_id             uuid,
    academic_year_id        uuid,
    academic_term_id        uuid        NOT NULL,
    curriculum_version_id   uuid        NOT NULL,
    curriculum_component_id uuid        NOT NULL,
    -- Business identifier, e.g. `CS201-T1-2026`; unique per institution and term among live offerings.
    offering_code           text        NOT NULL CHECK (offering_code ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]{0,39}$'),
    title                   text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- Copied from the curriculum component at creation (Domain B rows are never cached beyond this snapshot).
    course_code             text,
    category                text,
    credits                 numeric(7,2) NOT NULL CHECK (credits >= 0),
    delivery_mode           text        NOT NULL DEFAULT 'in_person'
        CHECK (delivery_mode IN ('in_person', 'online', 'hybrid')),
    -- Offering-wide seat cap (sections carry their own); NULL = unlimited.
    capacity                integer     CHECK (capacity IS NULL OR capacity >= 0),
    status                  text        NOT NULL DEFAULT 'draft'
        CONSTRAINT course_offering_status_check CHECK (status IN (
            'draft', 'pending_approval', 'approved', 'published', 'enrollment_open',
            'enrollment_closed', 'active', 'suspended', 'frozen', 'completed', 'archived',
            'cancelled', 'merged', 'split')),
    status_reason           text,
    status_changed_at       timestamptz NOT NULL DEFAULT now(),
    process_instance_id     text,
    -- Merge target (L3-D-011); split children point back with `split_from_id`.
    merged_into_id          uuid        REFERENCES academic.course_offering (id),
    split_from_id           uuid        REFERENCES academic.course_offering (id),
    version                 bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX course_offering_code_live
    ON academic.course_offering (tenant_id, institution_id, academic_term_id, lower(offering_code))
    WHERE status NOT IN ('cancelled', 'archived', 'merged', 'split');
CREATE INDEX course_offering_term_idx ON academic.course_offering (tenant_id, institution_id, academic_term_id);
CREATE INDEX course_offering_component_idx ON academic.course_offering (tenant_id, curriculum_component_id);

-- Teaching section of an offering.
CREATE TABLE academic.offering_section (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_code        text        NOT NULL CHECK (section_code ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,19}$'),
    campus_id           uuid,
    capacity            integer     CHECK (capacity IS NULL OR capacity >= 0),
    status              text        NOT NULL DEFAULT 'planned'
        CONSTRAINT offering_section_status_check CHECK (status IN (
            'planned', 'active', 'completed', 'cancelled', 'merged')),
    status_reason       text,
    status_changed_at   timestamptz NOT NULL DEFAULT now(),
    merged_into_id      uuid        REFERENCES academic.offering_section (id),
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX offering_section_code_live
    ON academic.offering_section (offering_id, lower(section_code))
    WHERE status NOT IN ('cancelled', 'merged');
CREATE INDEX offering_section_offering_idx ON academic.offering_section (tenant_id, offering_id);

-- Faculty Academic Assignment (L2-D02): who teaches a section, in which role.
CREATE TABLE academic.faculty_assignment (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    offering_id         uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id          uuid        NOT NULL REFERENCES academic.offering_section (id),
    -- Login subject of the faculty member (dev `dev|<loginId>`, Keycloak `sub`): the key of
    -- faculty-scoped permissions. People (Domain E, Batch 7) staff id once it exists.
    faculty_subject_ref text        NOT NULL CHECK (length(btrim(faculty_subject_ref)) BETWEEN 1 AND 200),
    faculty_name        text,
    staff_id            uuid,
    role                text        NOT NULL DEFAULT 'primary_instructor'
        CHECK (role IN ('primary_instructor', 'co_instructor', 'tutor', 'lab_instructor',
                        'teaching_assistant', 'course_coordinator')),
    -- Weekly contact hours counted against workload limits (4A governance).
    weekly_hours        numeric(5,2) CHECK (weekly_hours IS NULL OR weekly_hours >= 0),
    effective_from      date,
    effective_to        date,
    status              text        NOT NULL DEFAULT 'draft'
        CONSTRAINT faculty_assignment_status_check CHECK (status IN (
            'draft', 'submitted', 'under_review', 'approved', 'active', 'completed', 'archived',
            'rejected', 'withdrawn', 'revoked')),
    status_reason       text,
    status_changed_at   timestamptz NOT NULL DEFAULT now(),
    process_instance_id text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT faculty_assignment_dates CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to >= effective_from)
);
-- One live assignment per faculty member, section and role.
CREATE UNIQUE INDEX faculty_assignment_one_live
    ON academic.faculty_assignment (section_id, faculty_subject_ref, role)
    WHERE status IN ('draft', 'submitted', 'under_review', 'approved', 'active');
CREATE INDEX faculty_assignment_subject_idx ON academic.faculty_assignment (tenant_id, faculty_subject_ref, status);
CREATE INDEX faculty_assignment_section_idx ON academic.faculty_assignment (section_id);

-- A student's seat in a section (the roster 4B attendance / coursework and 4C use).
CREATE TABLE academic.section_enrolment (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL,
    offering_id          uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id           uuid        NOT NULL REFERENCES academic.offering_section (id),
    -- Domain C ids (plain uuids, validated through StudentDirectory at enrolment).
    student_id           uuid        NOT NULL,
    student_context_id   uuid        NOT NULL,
    -- Snapshot for rosters (no join into the student schema); refreshed by 4A when it re-validates.
    student_number       text        NOT NULL,
    display_name         text        NOT NULL,
    subject_ref          text,
    -- 3B registration line that justified the seat (RegistrationReader), NULL for overrides.
    registration_id      uuid,
    registration_line_id uuid,
    -- `registration` (gated by RegistrationReader), `override` (offering.enrolment.manage with reason), `waitlist`, `transfer`.
    origin               text        NOT NULL DEFAULT 'registration'
        CHECK (origin IN ('registration', 'override', 'waitlist', 'transfer')),
    status               text        NOT NULL DEFAULT 'enrolled'
        CONSTRAINT section_enrolment_status_check CHECK (status IN (
            'enrolled', 'dropped', 'withdrawn', 'transferred', 'completed')),
    status_reason        text,
    status_changed_at    timestamptz NOT NULL DEFAULT now(),
    transferred_from_id  uuid        REFERENCES academic.section_enrolment (id),
    enrolled_at          timestamptz NOT NULL DEFAULT now(),
    ended_at             timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
-- One seat per student and offering (and therefore per section).
CREATE UNIQUE INDEX section_enrolment_one_seat
    ON academic.section_enrolment (offering_id, student_id)
    WHERE status = 'enrolled';
CREATE INDEX section_enrolment_section_idx ON academic.section_enrolment (section_id, status);
CREATE INDEX section_enrolment_student_idx ON academic.section_enrolment (tenant_id, student_id, status);

-- Immutable status history of the four shared aggregates (written by shared::lifecycle).
CREATE TABLE academic.status_history (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    aggregate_type      text        NOT NULL CHECK (aggregate_type IN (
        'course_offering', 'offering_section', 'faculty_assignment', 'section_enrolment')),
    aggregate_id        uuid        NOT NULL,
    from_status         text,
    to_status           text        NOT NULL,
    reason              text,
    cause_event_type    text,
    process_instance_id text,
    actor               text,
    correlation_id      text,
    recorded_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX status_history_aggregate_idx ON academic.status_history (aggregate_id, recorded_at);
CREATE TRIGGER status_history_immutable
    BEFORE UPDATE OR DELETE ON academic.status_history
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- No physical deletion of the shared aggregates (history is kept through status).
CREATE TRIGGER course_offering_no_delete BEFORE DELETE ON academic.course_offering
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
CREATE TRIGGER offering_section_no_delete BEFORE DELETE ON academic.offering_section
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
CREATE TRIGGER faculty_assignment_no_delete BEFORE DELETE ON academic.faculty_assignment
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
CREATE TRIGGER section_enrolment_no_delete BEFORE DELETE ON academic.section_enrolment
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Archived / merged / split offerings are immutable.
CREATE FUNCTION academic.forbid_terminal_offering_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status IN ('archived', 'merged', 'split') THEN
        RAISE EXCEPTION 'course offering % is terminal (%) and immutable', OLD.id, OLD.status;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER course_offering_terminal_immutable
    BEFORE UPDATE ON academic.course_offering
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_terminal_offering_change();
