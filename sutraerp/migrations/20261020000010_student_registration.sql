-- Batch 3 slice 3B: academic registration (L2-C03, L3-C-020 … C-026).
--
-- Registration windows per academic term, a student's registration for one term with its
-- curriculum-component lines (regular / backlog / repeat / audit / elective), its immutable
-- status history, and registration holds. Domain B ids (institution, term, curriculum
-- component) are plain uuids validated through InstitutionReferencePort (ADR-L5-0036 §3.2).

CREATE TABLE student.registration_window (
    id               uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid        NOT NULL,
    institution_id   uuid        NOT NULL,
    academic_term_id uuid        NOT NULL,
    name             text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    -- Business-local dates (institution timezone), inclusive.
    opens_on         date        NOT NULL,
    closes_on        date        NOT NULL,
    min_credits      numeric(7,2) CHECK (min_credits IS NULL OR min_credits >= 0),
    max_credits      numeric(7,2) CHECK (max_credits IS NULL OR max_credits > 0),
    -- Key date of the calendar version the dates came from, when derived (L4-C-020-BH-001).
    key_date_id      uuid,
    status           text        NOT NULL DEFAULT 'scheduled'
        CONSTRAINT registration_window_status_check CHECK (status IN ('scheduled', 'open', 'closed', 'cancelled')),
    status_reason    text,
    opened_at        timestamptz,
    closed_at        timestamptz,
    version          bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT registration_window_dates CHECK (opens_on <= closes_on),
    CONSTRAINT registration_window_credits CHECK (min_credits IS NULL OR max_credits IS NULL OR min_credits <= max_credits)
);
-- At most one live (scheduled | open) window per term of an institution.
CREATE UNIQUE INDEX registration_window_one_live
    ON student.registration_window (tenant_id, institution_id, academic_term_id)
    WHERE status IN ('scheduled', 'open');
CREATE INDEX registration_window_inst_idx ON student.registration_window (tenant_id, institution_id, status);

CREATE TABLE student.registration (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    student_id          uuid        NOT NULL REFERENCES student.student (id),
    student_context_id  uuid        NOT NULL REFERENCES student.student_context (id),
    academic_term_id    uuid        NOT NULL,
    registration_window_id uuid     NOT NULL REFERENCES student.registration_window (id),
    -- Programme term (semester) the registration is for (context.current_term_no at creation).
    term_no             integer     NOT NULL CHECK (term_no >= 1),
    curriculum_version_id uuid,
    status              text        NOT NULL DEFAULT 'draft'
        CONSTRAINT registration_status_check CHECK (status IN (
            'draft', 'submitted', 'returned', 'approved', 'registered', 'finalised', 'cancelled', 'archived')),
    status_reason       text,
    -- `staff` or `self` (who created it).
    origin              text        NOT NULL DEFAULT 'staff' CHECK (origin IN ('staff', 'self')),
    process_instance_id text,
    submitted_at        timestamptz,
    registered_at       timestamptz,
    finalised_at        timestamptz,
    cancelled_at        timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
-- One non-cancelled registration per context and term.
CREATE UNIQUE INDEX registration_one_per_term
    ON student.registration (tenant_id, student_context_id, academic_term_id)
    WHERE status NOT IN ('cancelled', 'archived');
CREATE INDEX registration_student_idx ON student.registration (tenant_id, student_id, created_at DESC);
CREATE INDEX registration_window_idx ON student.registration (registration_window_id, status);

CREATE TABLE student.registration_line (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    registration_id         uuid        NOT NULL REFERENCES student.registration (id),
    curriculum_component_id uuid        NOT NULL,
    kind                    text        NOT NULL CHECK (kind IN ('regular', 'backlog', 'repeat', 'audit', 'elective')),
    status                  text        NOT NULL DEFAULT 'selected'
        CONSTRAINT registration_line_status_check CHECK (status IN ('selected', 'registered', 'dropped', 'withdrawn')),
    credits                 numeric(7,2) NOT NULL CHECK (credits >= 0),
    -- Programme term of the component (curriculum), copied for display/history.
    component_term_no       integer     NOT NULL,
    slot_code               text        NOT NULL,
    title                   text        NOT NULL,
    -- Add/drop after registration (L3-C-022).
    added_after_registration boolean    NOT NULL DEFAULT false,
    dropped_at              timestamptz,
    drop_reason             text,
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX registration_line_one_live
    ON student.registration_line (registration_id, curriculum_component_id)
    WHERE status IN ('selected', 'registered');
CREATE INDEX registration_line_reg_idx ON student.registration_line (registration_id);
CREATE INDEX registration_line_component_idx ON student.registration_line (tenant_id, curriculum_component_id);

-- Immutable registration history (L2-C03 §16.10).
CREATE TABLE student.registration_status_history (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    registration_id     uuid        NOT NULL REFERENCES student.registration (id),
    from_status         text,
    to_status           text        NOT NULL,
    action              text        NOT NULL,
    reason              text,
    process_instance_id text,
    actor               text,
    correlation_id      text,
    occurred_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX registration_status_history_idx ON student.registration_status_history (registration_id, occurred_at);
CREATE TRIGGER registration_status_history_immutable
    BEFORE UPDATE OR DELETE ON student.registration_status_history
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.registration_hold (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid        NOT NULL,
    student_id         uuid        NOT NULL REFERENCES student.student (id),
    student_context_id uuid        REFERENCES student.student_context (id),
    source             text        NOT NULL CHECK (source IN ('academic', 'administrative', 'discipline', 'documents', 'finance')),
    reason             text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 500),
    external_reference text,
    status             text        NOT NULL DEFAULT 'active'
        CONSTRAINT registration_hold_status_check CHECK (status IN ('active', 'released')),
    placed_by          text,
    placed_at          timestamptz NOT NULL DEFAULT now(),
    released_by        text,
    released_at        timestamptz,
    release_reason     text,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX registration_hold_student_idx ON student.registration_hold (tenant_id, student_id, status);
