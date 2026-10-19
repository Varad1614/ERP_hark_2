-- Batch 3 foundation (contract doc docs/batch3-contracts.md §3).
--
-- The shared Student aggregate that every Batch 3 slice references: the tenant-scoped Student
-- persona (L2-C02 §6, L1 amendment 2026-08-26), the per-Institute StudentContext
-- (L2-C02 §11.10) and their immutable status history. Written by the foundation helper
-- `sutra_student::shared::lifecycle` (status changes) and by slice 3A (creation, attributes);
-- read by every slice.
--
-- Domain B data (institution, campus, org unit, programme, curriculum version, term) is
-- referenced by plain `uuid` columns: Domain C never joins or FKs the `institution` schema
-- (ADR-L5-0036 §3.2); it validates ids through `InstitutionReferencePort`.
--
-- Reserved migration ranges (one file per change, never edit an applied file):
--   3A  20261020000001 .. 20261020000009
--   3B  20261020000010 .. 20261020000019
--   3C  20261020000020 .. 20261020000029

CREATE TABLE student.student (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    -- Canonical Person (Platform A02) once it exists; Domain C never owns the Person.
    person_id            uuid,
    -- Institution-configurable numbering; immutable once assigned (L2-C02 §6.6).
    student_number       text        NOT NULL CHECK (student_number ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]{0,39}$'),
    given_name           text        NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 100),
    middle_name          text,
    family_name          text,
    preferred_name       text,
    date_of_birth        date,
    gender               text,
    primary_email        text,
    primary_phone        text,
    -- Login subject linked for self-service (dev: `dev|<loginId>`, Keycloak: `sub`).
    subject_ref          text,
    -- Admission that created the persona (3A table; plain uuid, no cross-slice FK). NULL only
    -- for seeded/migrated records.
    origin_admission_id  uuid,
    -- Duplicate resolution (L3-C-017): the archived duplicate points at the survivor.
    merged_into_id       uuid        REFERENCES student.student (id),
    status               text        NOT NULL DEFAULT 'created'
        CONSTRAINT student_status_check CHECK (status IN (
            'created', 'activated', 'enrolled', 'academically_assigned', 'active',
            'suspended', 'qualification_completed', 'archived')),
    status_changed_at    timestamptz NOT NULL DEFAULT now(),
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT student_number_unique UNIQUE (tenant_id, student_number)
);
CREATE UNIQUE INDEX student_subject_unique
    ON student.student (tenant_id, subject_ref) WHERE subject_ref IS NOT NULL AND status <> 'archived';
CREATE INDEX student_tenant_status_idx ON student.student (tenant_id, status);
CREATE INDEX student_name_idx ON student.student (tenant_id, lower(given_name), lower(family_name));

CREATE TABLE student.student_context (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    -- Domain B references (plain uuids, validated through InstitutionReferencePort).
    institution_id         uuid        NOT NULL,
    campus_id              uuid,
    programme_id           uuid,
    curriculum_version_id  uuid,
    org_unit_id            uuid,
    admission_term_id      uuid,
    admission_year_id      uuid,
    -- Placement inside the programme (L2-C02 §6.4 Academic Assignment).
    batch_code             text,
    section_code           text,
    -- 1-based term (semester/trimester/year) the student is in; progression (3B) advances it.
    current_term_no        integer     CHECK (current_term_no >= 1),
    enrolment_number       text,
    roll_number            text,
    enrolment_type         text        NOT NULL DEFAULT 'regular'
        CONSTRAINT student_context_enrolment_type_check CHECK (enrolment_type IN (
            'regular', 'lateral_entry', 'readmission', 'transfer_in', 're_entry')),
    enrolled_on            date,
    -- Admission (3A table) that opened this context; plain uuid.
    admission_id           uuid,
    -- Previous context for readmission / inter-institute transfer / re-entry.
    predecessor_context_id uuid        REFERENCES student.student_context (id),
    status                 text        NOT NULL DEFAULT 'pending_activation'
        CONSTRAINT student_context_status_check CHECK (status IN (
            'pending_activation', 'active', 'suspended', 'qualification_completed',
            'withdrawn', 'transferred', 'discontinued', 'terminated')),
    status_reason          text,
    status_changed_at      timestamptz NOT NULL DEFAULT now(),
    -- Business-local date (institution timezone) the context became terminal.
    closed_on              date,
    version                bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- SC-INV-001 / SC-INV-004: at most one live context per student and institution.
CREATE UNIQUE INDEX student_context_one_live
    ON student.student_context (tenant_id, student_id, institution_id)
    WHERE status IN ('pending_activation', 'active', 'suspended');
CREATE UNIQUE INDEX student_context_enrolment_number_unique
    ON student.student_context (tenant_id, institution_id, enrolment_number)
    WHERE enrolment_number IS NOT NULL;
CREATE INDEX student_context_institution_idx
    ON student.student_context (tenant_id, institution_id, status);
CREATE INDEX student_context_programme_idx
    ON student.student_context (tenant_id, programme_id) WHERE programme_id IS NOT NULL;
CREATE INDEX student_context_student_idx ON student.student_context (student_id);

-- Immutable status history of personas and contexts (L2-C02 §6.16 audit attributes).
CREATE TABLE student.student_status_history (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    student_id           uuid        NOT NULL REFERENCES student.student (id),
    student_context_id   uuid        REFERENCES student.student_context (id),
    institution_id       uuid        NOT NULL,
    scope                text        NOT NULL CHECK (scope IN ('student', 'context')),
    from_status          text,
    to_status            text        NOT NULL,
    reason               text,
    cause_event_type     text        NOT NULL,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    occurred_at          timestamptz NOT NULL DEFAULT now(),
    CHECK ((scope = 'context') = (student_context_id IS NOT NULL))
);
CREATE INDEX student_status_history_student_idx
    ON student.student_status_history (student_id, occurred_at);

-- History rows are append-only.
CREATE FUNCTION student.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of student.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;
CREATE TRIGGER student_status_history_immutable
    BEFORE UPDATE OR DELETE ON student.student_status_history
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

-- Terminal contexts are immutable (SC-INV-003).
CREATE FUNCTION student.forbid_terminal_context_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status IN ('qualification_completed', 'withdrawn', 'transferred', 'discontinued', 'terminated') THEN
        RAISE EXCEPTION 'student context % is terminal (%) and immutable', OLD.id, OLD.status;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER student_context_terminal_immutable
    BEFORE UPDATE ON student.student_context
    FOR EACH ROW EXECUTE FUNCTION student.forbid_terminal_context_change();
-- Students and contexts are never physically removed (L2-C02 §6.6).
CREATE TRIGGER student_context_no_delete
    BEFORE DELETE ON student.student_context
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
CREATE TRIGGER student_no_delete
    BEFORE DELETE ON student.student
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
