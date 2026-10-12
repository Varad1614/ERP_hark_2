-- Batch 3 slice 3A: student record (L2-C02, L3-C-008…019; docs/batch3-contracts.md §3.3).

-- Number allocation (student numbers per tenant and year, enrolment numbers per institution).
CREATE TABLE student.student_number_sequence (
    tenant_id   uuid    NOT NULL,
    scope_key   text    NOT NULL,
    next_value  bigint  NOT NULL DEFAULT 1 CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, scope_key)
);

-- Append-only record history: previous/current enrolment, assignment, placement and profile
-- values (L4-C-008-BH-003, C-009-BH-004, C-012-BH-004, C-014-BH-005).
CREATE TABLE student.student_record_history (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    student_id          uuid        NOT NULL REFERENCES student.student (id),
    student_context_id  uuid        REFERENCES student.student_context (id),
    kind                text        NOT NULL CHECK (kind IN (
        'enrolment', 'assignment', 'transfer', 'profile', 'identity', 'merge', 'communication')),
    before_values       jsonb       NOT NULL DEFAULT '{}',
    after_values        jsonb       NOT NULL DEFAULT '{}',
    reason              text,
    actor               text,
    correlation_id      text,
    occurred_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX student_record_history_idx ON student.student_record_history (student_id, occurred_at);
CREATE TRIGGER student_record_history_immutable
    BEFORE UPDATE OR DELETE ON student.student_record_history
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

-- Suspension episodes (L3-C-010/011): reason and restoration kept forever.
CREATE TABLE student.student_suspension (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    student_id           uuid        NOT NULL REFERENCES student.student (id),
    student_context_id   uuid        NOT NULL REFERENCES student.student_context (id),
    institution_id       uuid        NOT NULL,
    category             text        NOT NULL DEFAULT 'administrative'
        CHECK (category IN ('administrative', 'disciplinary', 'financial', 'academic', 'medical', 'other')),
    reason               text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    suspended_on         date        NOT NULL,
    review_on            date,
    status               text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'restored')),
    restored_on          date,
    restoration_reason   text,
    process_instance_id  text,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX student_suspension_one_active
    ON student.student_suspension (student_context_id) WHERE status = 'active';

-- Communication preferences (L3-C-015). One row per student; a missing row means defaults.
CREATE TABLE student.communication_preference (
    student_id         uuid        PRIMARY KEY REFERENCES student.student (id),
    tenant_id          uuid        NOT NULL,
    preferred_channel  text        NOT NULL DEFAULT 'email'
        CHECK (preferred_channel IN ('email', 'sms', 'in_app', 'whatsapp', 'post')),
    language           text        NOT NULL DEFAULT 'en' CHECK (language ~ '^[a-z]{2,3}(-[A-Z]{2})?$'),
    email_opt_in       boolean     NOT NULL DEFAULT true,
    sms_opt_in         boolean     NOT NULL DEFAULT false,
    whatsapp_opt_in    boolean     NOT NULL DEFAULT false,
    marketing_opt_in   boolean     NOT NULL DEFAULT false,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

-- Documents held on the student's record.
CREATE TABLE student.student_document (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    institution_id         uuid,
    document_type          text        NOT NULL CHECK (document_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    title                  text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    file_ref               text,
    status                 text        NOT NULL DEFAULT 'submitted'
        CONSTRAINT student_document_status_check CHECK (status IN (
            'submitted', 'verified', 'rejected', 'waived', 'superseded')),
    source                 text        NOT NULL DEFAULT 'staff'
        CHECK (source IN ('admission', 'staff', 'request', 'transfer')),
    admission_document_id  uuid        REFERENCES student.admission_document (id),
    note                   text,
    version                bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX student_document_student_idx ON student.student_document (tenant_id, student_id);

-- Document / certificate requests (L3-C-018). Generation stays with Document Services.
CREATE TABLE student.document_request (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    student_id           uuid        NOT NULL REFERENCES student.student (id),
    student_context_id   uuid        REFERENCES student.student_context (id),
    institution_id       uuid        NOT NULL,
    document_type        text        NOT NULL CHECK (document_type IN (
        'bonafide_certificate', 'transcript_copy', 'migration_certificate',
        'character_certificate', 'conduct_certificate', 'enrolment_letter', 'id_card_reissue')),
    purpose              text        CHECK (length(purpose) <= 500),
    copies               integer     NOT NULL DEFAULT 1 CHECK (copies BETWEEN 1 AND 10),
    channel              text        NOT NULL CHECK (channel IN ('self_service', 'staff')),
    requested_by         text,
    status               text        NOT NULL DEFAULT 'requested'
        CONSTRAINT document_request_status_check CHECK (status IN (
            'requested', 'in_progress', 'issued', 'rejected', 'cancelled')),
    status_reason        text,
    issued_reference     text,
    issued_on            date,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
-- No duplicate active request of the same type (L4-C-018-BH-001).
CREATE UNIQUE INDEX document_request_one_active
    ON student.document_request (tenant_id, student_id, document_type)
    WHERE status IN ('requested', 'in_progress');
CREATE INDEX document_request_institution_idx
    ON student.document_request (tenant_id, institution_id, status);

-- Governed identity corrections (L3-C-016).
CREATE TABLE student.identity_correction (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    student_id           uuid        NOT NULL REFERENCES student.student (id),
    institution_id       uuid        NOT NULL,
    changes              jsonb       NOT NULL,
    previous             jsonb,
    evidence             text        NOT NULL CHECK (length(btrim(evidence)) BETWEEN 1 AND 2000),
    reason               text,
    status               text        NOT NULL DEFAULT 'requested'
        CONSTRAINT identity_correction_status_check CHECK (status IN (
            'requested', 'under_review', 'approved', 'rejected', 'completed', 'withdrawn')),
    decision_reason      text,
    process_instance_id  text,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX identity_correction_student_idx ON student.identity_correction (tenant_id, student_id);

-- Duplicate student resolution (L3-C-017).
CREATE TABLE student.duplicate_case (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    survivor_student_id   uuid        NOT NULL REFERENCES student.student (id),
    duplicate_student_id  uuid        NOT NULL REFERENCES student.student (id),
    institution_id        uuid        NOT NULL,
    evidence              text        NOT NULL CHECK (length(btrim(evidence)) BETWEEN 1 AND 2000),
    status                text        NOT NULL DEFAULT 'requested'
        CONSTRAINT duplicate_case_status_check CHECK (status IN (
            'requested', 'under_review', 'approved', 'rejected', 'completed', 'withdrawn')),
    decision_reason       text,
    process_instance_id   text,
    moved_context_ids     uuid[]      NOT NULL DEFAULT '{}',
    version               bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (survivor_student_id <> duplicate_student_id)
);
CREATE UNIQUE INDEX duplicate_case_open_pair
    ON student.duplicate_case (tenant_id, LEAST(survivor_student_id, duplicate_student_id),
        GREATEST(survivor_student_id, duplicate_student_id))
    WHERE status IN ('requested', 'under_review', 'approved');

-- Intra-/inter-institute transfers (L3-C-012/013).
CREATE TABLE student.student_transfer (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    student_id            uuid        NOT NULL REFERENCES student.student (id),
    kind                  text        NOT NULL CHECK (kind IN ('intra_institute', 'inter_institute')),
    from_context_id       uuid        NOT NULL REFERENCES student.student_context (id),
    to_context_id         uuid        REFERENCES student.student_context (id),
    from_institution_id   uuid        NOT NULL,
    to_institution_id     uuid        NOT NULL,
    to_campus_id          uuid,
    to_org_unit_id        uuid,
    to_programme_id       uuid,
    to_curriculum_version_id uuid,
    to_batch_code         text,
    to_section_code       text,
    effective_on          date        NOT NULL,
    reason                text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    status                text        NOT NULL DEFAULT 'requested'
        CONSTRAINT student_transfer_status_check CHECK (status IN (
            'requested', 'under_review', 'approved', 'rejected', 'completed', 'withdrawn')),
    decision_reason       text,
    process_instance_id   text,
    version               bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'intra_institute') = (from_institution_id = to_institution_id))
);
CREATE UNIQUE INDEX student_transfer_one_open
    ON student.student_transfer (from_context_id) WHERE status IN ('requested', 'under_review', 'approved');
