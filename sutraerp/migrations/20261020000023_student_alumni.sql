-- Batch 3 slice 3C: student → alumni transition, correction and archival (L2-C08 + boundary
-- clarification 2026-08-26, L3-C-072 … C-074). The H06 Alumni Profile / engagement relationship
-- is downstream (consumes `alumni_transition.completed`).

CREATE TABLE student.alumni_transition (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    -- `qualification` (conferred qualification) or `eligible_exit` (completed withdrawal / transfer exit).
    basis                  text        CHECK (basis IN ('qualification', 'eligible_exit')),
    conferred_qualification_id uuid    REFERENCES student.conferred_qualification (id),
    exit_id                uuid        REFERENCES student.student_exit (id),
    status                 text        NOT NULL CHECK (status IN ('created', 'pending_verification',
                                           'verified', 'pending_approval', 'approved',
                                           'transitioning', 'activated', 'completed', 'archived',
                                           'rejected')),
    graduation_year        integer     CHECK (graduation_year BETWEEN 1900 AND 2200),
    alumni_email           text,
    directory_consent      boolean     NOT NULL DEFAULT false,
    eligibility_result     jsonb,
    process_instance_id    text,
    decision_reason        text,
    activated_at           timestamptz,
    completed_at           timestamptz,
    archived_at            timestamptz,
    correction_count       integer     NOT NULL DEFAULT 0,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX alumni_transition_one_per_context
    ON student.alumni_transition (tenant_id, student_context_id) WHERE status <> 'rejected';
CREATE INDEX alumni_transition_student_idx ON student.alumni_transition (tenant_id, student_id);

-- Corrections append a version; the earlier values are preserved here (L4-C-073).
CREATE TABLE student.alumni_transition_correction (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    transition_id          uuid        NOT NULL REFERENCES student.alumni_transition (id),
    transition_version     bigint      NOT NULL,
    changed_fields         text[]      NOT NULL,
    previous               jsonb       NOT NULL,
    corrected              jsonb       NOT NULL,
    reason                 text        NOT NULL,
    actor                  text,
    recorded_at            timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER alumni_transition_correction_immutable
    BEFORE UPDATE OR DELETE ON student.alumni_transition_correction
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
CREATE TRIGGER alumni_transition_no_delete
    BEFORE DELETE ON student.alumni_transition
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
