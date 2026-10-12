-- Batch 3 slice 3C: qualification completion, conferment, correction/revocation, deferral,
-- student exits and exit clearance (L2-C05 + MEME amendment, L3-C-039 … C-050).
-- Conventions: docs/batch3-contracts.md §3. No FK into another schema; FKs only to the
-- foundation tables and to 3C's own tables. `eligibility_id` (3B qualification eligibility) is a
-- plain uuid (no FK across slices).

CREATE TABLE student.qualification_completion (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    programme_id           uuid,
    curriculum_version_id  uuid,
    -- `completion` (programme completion) or `meme_exit` (NEP intermediate exit, amendment 2026-08-22).
    pathway                text        NOT NULL CHECK (pathway IN ('completion', 'meme_exit')),
    outcome                text        NOT NULL CHECK (outcome IN ('certificate', 'diploma',
                                           'advanced_diploma', 'degree', 'degree_with_research')),
    status                 text        NOT NULL CHECK (status IN ('eligibility_evaluation',
                                           'degree_audit', 'institutional_clearance',
                                           'approval_pending', 'approved', 'awarded',
                                           'record_finalized', 'exit_completed', 'deferred',
                                           'cancelled')),
    -- Clearance items opened when institutional clearance starts.
    requested_clearances   text[]      NOT NULL DEFAULT '{}',
    eligibility_id         uuid,
    -- Evidence of the last eligibility evaluation ({eligible, source, creditsEarned, requiredCredits, unmet[]}).
    eligibility_result     jsonb,
    -- Degree / qualification audit basis ({passed, deficiencies[], note, creditsEarned, requiredCredits}).
    audit_result           jsonb,
    process_instance_id    text,
    decision_reason        text,
    conferred_qualification_id uuid,
    conferred_on           date,
    finalized_on           date,
    exit_id                uuid,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- At most one open completion per StudentContext (a conflicting terminal outcome is refused).
CREATE UNIQUE INDEX qualification_completion_one_open
    ON student.qualification_completion (tenant_id, student_context_id)
    WHERE status NOT IN ('exit_completed', 'cancelled');
CREATE INDEX qualification_completion_student_idx
    ON student.qualification_completion (tenant_id, student_id);

CREATE TABLE student.conferred_qualification (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    completion_id          uuid        NOT NULL REFERENCES student.qualification_completion (id),
    programme_id           uuid,
    outcome                text        NOT NULL CHECK (outcome IN ('certificate', 'diploma',
                                           'advanced_diploma', 'degree', 'degree_with_research')),
    title                  text        NOT NULL,
    conferred_on           date        NOT NULL,
    status                 text        NOT NULL CHECK (status IN ('conferred', 'corrected', 'revoked')),
    -- The conferred version this one corrects (corrections never rewrite the earlier row).
    supersedes_id          uuid        REFERENCES student.conferred_qualification (id),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX conferred_qualification_student_idx
    ON student.conferred_qualification (tenant_id, student_id);

-- Correction / revocation cases of a conferred qualification (L3-C-050, CaseStatus).
CREATE TABLE student.qualification_correction (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    conferred_qualification_id uuid    NOT NULL REFERENCES student.conferred_qualification (id),
    kind                   text        NOT NULL CHECK (kind IN ('correction', 'revocation')),
    status                 text        NOT NULL CHECK (status IN ('requested', 'under_review',
                                           'approved', 'rejected', 'completed', 'withdrawn')),
    -- Proposed values ({outcome?, title?, conferredOn?}) for a correction.
    proposed               jsonb       NOT NULL DEFAULT '{}'::jsonb,
    reason                 text        NOT NULL,
    evidence_ref           text,
    -- `corrected`, `revoked` or `no_change` once decided.
    resolution             text        CHECK (resolution IN ('corrected', 'revoked', 'no_change')),
    decision_reason        text,
    result_qualification_id uuid       REFERENCES student.conferred_qualification (id),
    process_instance_id    text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX qualification_correction_one_open
    ON student.qualification_correction (tenant_id, conferred_qualification_id)
    WHERE status IN ('requested', 'under_review', 'approved');

-- Deferral + remediation of a qualification completion (L3-C-049, CaseStatus:
-- `approved` = deferral in effect, `completed` = remediated and resumed, `withdrawn` = cancelled with the completion).
CREATE TABLE student.completion_deferral (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    completion_id          uuid        NOT NULL REFERENCES student.qualification_completion (id),
    -- The completion state the deferral interrupted (resumption re-enters eligibility evaluation).
    deferred_from          text        NOT NULL,
    reason                 text        NOT NULL,
    remediation            text,
    review_on              date,
    status                 text        NOT NULL CHECK (status IN ('requested', 'under_review',
                                           'approved', 'rejected', 'completed', 'withdrawn')),
    resolution_note        text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX completion_deferral_one_open
    ON student.completion_deferral (tenant_id, completion_id) WHERE status = 'approved';

CREATE TABLE student.student_exit (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    kind                   text        NOT NULL CHECK (kind IN ('completion', 'meme_exit',
                                           'withdrawal', 'transfer', 'discontinuation',
                                           'termination', 'administrative_closure', 'deceased')),
    status                 text        NOT NULL CHECK (status IN ('initiated', 'clearance_pending',
                                           'approval_pending', 'approved', 'completed',
                                           'rejected', 'cancelled')),
    reason                 text        NOT NULL,
    requested_effective_on date,
    effective_on           date,
    -- Transfer-out (external): the receiving institution as stated by the student / authority.
    destination_institution text,
    -- Deceased closure: reference of the verified notification (certificate number, authority letter).
    evidence_ref           text,
    completion_id          uuid        REFERENCES student.qualification_completion (id),
    process_instance_id    text,
    decision_reason        text,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- One open exit per StudentContext: a competing terminal outcome never creates a second exit.
CREATE UNIQUE INDEX student_exit_one_open
    ON student.student_exit (tenant_id, student_context_id)
    WHERE status NOT IN ('rejected', 'cancelled');
CREATE INDEX student_exit_institution_idx ON student.student_exit (tenant_id, institution_id, status);

-- Institutional clearance items of an exit or of a qualification completion (L3-C-046).
CREATE TABLE student.exit_clearance (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    exit_id                uuid        REFERENCES student.student_exit (id),
    completion_id          uuid        REFERENCES student.qualification_completion (id),
    item                   text        NOT NULL CHECK (item IN ('finance', 'library', 'hostel',
                                           'department', 'laboratory', 'sports', 'administration',
                                           'examination')),
    status                 text        NOT NULL CHECK (status IN ('pending', 'cleared', 'waived', 'blocked')),
    note                   text,
    resolved_by            text,
    resolved_at            timestamptz,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK ((exit_id IS NULL) <> (completion_id IS NULL))
);
CREATE UNIQUE INDEX exit_clearance_item_per_exit
    ON student.exit_clearance (exit_id, item) WHERE exit_id IS NOT NULL;
CREATE UNIQUE INDEX exit_clearance_item_per_completion
    ON student.exit_clearance (completion_id, item) WHERE completion_id IS NOT NULL;

-- Conferred qualifications and correction cases are never physically removed.
CREATE TRIGGER conferred_qualification_no_delete
    BEFORE DELETE ON student.conferred_qualification
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
CREATE TRIGGER qualification_correction_no_delete
    BEFORE DELETE ON student.qualification_correction
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
CREATE TRIGGER student_exit_no_delete
    BEFORE DELETE ON student.student_exit
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
