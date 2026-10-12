-- Batch 3 slice 3A: admissions (L2-C01, L3-C-001…007; docs/batch3-contracts.md §3.3).
--
-- Admission application (candidate intake) → Admission Record (created when an offer is made)
-- → documents + verification → approval decision (WorkflowPort) → financial clearance (Finance
-- evidence through FinanceClearancePort) → confirmation → student created → completed.
-- Domain B ids (institution, programme, curriculum version, term, campus) are plain uuids.

CREATE TABLE student.admission_application (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    institution_id        uuid        NOT NULL,
    programme_id          uuid        NOT NULL,
    curriculum_version_id uuid,
    campus_id             uuid,
    admission_term_id     uuid,
    admission_year_id     uuid,
    -- Applicant (no Person aggregate yet: contract §12).
    given_name            text        NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 100),
    middle_name           text,
    family_name           text,
    date_of_birth         date,
    gender                text,
    email                 text,
    phone                 text,
    -- Login subject of the applicant, carried to the Student for self-service.
    subject_ref           text,
    prior_qualification   text,
    notes                 text,
    status                text        NOT NULL DEFAULT 'draft'
        CONSTRAINT admission_application_status_check CHECK (status IN (
            'draft', 'submitted', 'under_review', 'offered', 'rejected', 'withdrawn')),
    status_reason         text,
    submitted_at          timestamptz,
    decided_at            timestamptz,
    admission_id          uuid,
    version               bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX admission_application_institution_idx
    ON student.admission_application (tenant_id, institution_id, status);

CREATE TABLE student.admission (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    application_id         uuid        REFERENCES student.admission_application (id),
    programme_id           uuid        NOT NULL,
    curriculum_version_id  uuid        NOT NULL,
    campus_id              uuid,
    admission_term_id      uuid,
    admission_year_id      uuid,
    -- Applicant snapshot (copied from the application, or from the Student on readmission).
    given_name             text        NOT NULL,
    middle_name            text,
    family_name            text,
    date_of_birth          date,
    gender                 text,
    email                  text,
    phone                  text,
    subject_ref            text,
    -- Business-local date (institution timezone): after it the un-actioned offer expires.
    offer_expires_on       date,
    offer_accepted_at      timestamptz,
    -- Documentary requirements fixed when the offer is made (L4-C-007-BH-001).
    required_documents     text[]      NOT NULL DEFAULT '{}',
    enrolment_type         text        NOT NULL DEFAULT 'regular'
        CHECK (enrolment_type IN ('regular', 'lateral_entry', 'readmission', 'transfer_in', 're_entry')),
    status                 text        NOT NULL DEFAULT 'initiated'
        CONSTRAINT admission_status_check CHECK (status IN (
            'initiated', 'documents_submitted', 'verification_completed', 'approval_pending',
            'approved', 'financially_cleared', 'confirmed', 'student_created', 'completed',
            'cancelled', 'withdrawn', 'revoked', 'expired')),
    status_reason          text,
    process_instance_id    text,
    approval_decision      text        CHECK (approval_decision IN ('approve', 'reject')),
    approval_reason        text,
    approved_by            text,
    -- Finance-owned evidence the clearance decision was based on (amendment 2026-10-05).
    finance_evidence       jsonb,
    revocation_evidence    text,
    student_id             uuid        REFERENCES student.student (id),
    student_context_id     uuid        REFERENCES student.student_context (id),
    -- Readmission (L3-C-005): the historical admission this one reopens.
    previous_admission_id  uuid        REFERENCES student.admission (id),
    version                bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- One admission per application.
CREATE UNIQUE INDEX admission_application_unique
    ON student.admission (application_id) WHERE application_id IS NOT NULL;
-- No duplicate open admission for the same existing student in an institution (L4-C-005-BH-004).
CREATE UNIQUE INDEX admission_open_per_student
    ON student.admission (tenant_id, institution_id, student_id)
    WHERE student_id IS NOT NULL AND status IN ('initiated', 'documents_submitted',
        'verification_completed', 'approval_pending', 'approved', 'financially_cleared', 'confirmed');
CREATE INDEX admission_institution_idx ON student.admission (tenant_id, institution_id, status);
CREATE INDEX admission_offer_expiry_idx
    ON student.admission (offer_expires_on) WHERE status = 'initiated';
ALTER TABLE student.admission_application
    ADD CONSTRAINT admission_application_admission_fk
    FOREIGN KEY (admission_id) REFERENCES student.admission (id);

CREATE TABLE student.admission_document (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    admission_id    uuid        NOT NULL REFERENCES student.admission (id),
    document_type   text        NOT NULL CHECK (document_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    title           text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- Reference into document storage (Document Services own the file; we keep the pointer).
    file_ref        text,
    status          text        NOT NULL DEFAULT 'submitted'
        CONSTRAINT admission_document_status_check CHECK (status IN (
            'submitted', 'verified', 'rejected', 'waived', 'superseded')),
    note            text,
    verified_by     text,
    verified_at     timestamptz,
    version         bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX admission_document_admission_idx ON student.admission_document (admission_id);

-- Append-only lifecycle history of admission records.
CREATE TABLE student.admission_status_history (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    admission_id         uuid        NOT NULL REFERENCES student.admission (id),
    from_status          text,
    to_status            text        NOT NULL,
    reason               text,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    occurred_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX admission_status_history_idx
    ON student.admission_status_history (admission_id, occurred_at);
CREATE TRIGGER admission_status_history_immutable
    BEFORE UPDATE OR DELETE ON student.admission_status_history
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
