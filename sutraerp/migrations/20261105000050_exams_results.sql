-- Batch 6 slice 6C — results (L2-I09 … I13, I16): result batches, verification, governed result
-- decisions (correction / withdrawal), publications + items, grade cards, revaluation / rechecking
-- requests, supplementary eligibility, completion evaluations and session closure.
-- Shared rows (term_result(_line), paper_mark, exam_session, exam_registration) are written only
-- through the foundation helpers (contract §3.3); these are 6C's own aggregates.

-- ------------------------------------------------------------------ result batch (L2-I09 §20)
CREATE TABLE exams.result_batch (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL REFERENCES exams.exam_session(id),
    status               text         NOT NULL DEFAULT 'pending' CHECK (status IN (
                             'pending', 'aggregated', 'rule_processing', 'computed', 'validated',
                             'approved', 'finalised', 'ready_for_publication', 'closed', 'archived')),
    status_reason        text,
    run_count            integer      NOT NULL DEFAULT 0,
    candidates           integer      NOT NULL DEFAULT 0,
    computed_count       integer      NOT NULL DEFAULT 0,
    withheld_count       integer      NOT NULL DEFAULT 0,
    unchanged_count      integer      NOT NULL DEFAULT 0,
    computed_at          timestamptz,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
-- One live computation batch per session (L3-I-093: duplicate runs collapse onto it).
CREATE UNIQUE INDEX result_batch_one_live ON exams.result_batch (session_id)
    WHERE status NOT IN ('closed', 'archived');
CREATE INDEX result_batch_tenant_idx ON exams.result_batch (tenant_id, session_id);

-- Verification evidence of a batch (append-only).
CREATE TABLE exams.result_verification (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    result_batch_id      uuid         NOT NULL REFERENCES exams.result_batch(id),
    verified_count       integer      NOT NULL,
    withheld_count       integer      NOT NULL,
    discrepancies        jsonb        NOT NULL DEFAULT '[]'::jsonb,
    notes                text,
    verified_by          text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.result_verification FROM sutra_exams;
CREATE TRIGGER result_verification_append_only BEFORE UPDATE OR DELETE ON exams.result_verification
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- Governed decisions on one result (approved marks correction L3-I-088/117, withdrawal L3-I-109).
CREATE TABLE exams.result_decision (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL REFERENCES exams.exam_session(id),
    result_id            uuid         NOT NULL REFERENCES exams.term_result(id),
    kind                 text         NOT NULL CHECK (kind IN ('correction', 'withdrawal')),
    paper_id             uuid         REFERENCES exams.exam_paper(id),
    internal_marks       numeric(7,2),
    external_marks       numeric(7,2),
    grace_marks          numeric(7,2),
    reason               text         NOT NULL,
    status               text         NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected')),
    decision_reason      text,
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX result_decision_one_pending ON exams.result_decision (result_id, kind)
    WHERE status = 'pending';

-- ------------------------------------------------------------------ publication (L2-I10 §20)
CREATE TABLE exams.publication (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL REFERENCES exams.exam_session(id),
    status               text         NOT NULL DEFAULT 'planned' CHECK (status IN (
                             'planned', 'scheduled', 'authorised', 'ready', 'published', 'accessible',
                             'delayed', 'cancelled', 'failed', 'withdrawn', 'closed', 'archived')),
    status_reason        text,
    scheduled_on         date,
    delayed_until        date,
    authorised_at        timestamptz,
    published_at         timestamptz,
    attempts             integer      NOT NULL DEFAULT 0,
    published_count      integer      NOT NULL DEFAULT 0,
    skipped_count        integer      NOT NULL DEFAULT 0,
    last_error           text,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
-- One open publication per session at a time (republication = a new one after it completes).
CREATE UNIQUE INDEX publication_one_open ON exams.publication (session_id)
    WHERE status IN ('planned', 'scheduled', 'authorised', 'ready', 'delayed', 'failed');
CREATE INDEX publication_tenant_idx ON exams.publication (tenant_id, session_id);

CREATE TABLE exams.publication_item (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    publication_id       uuid         NOT NULL REFERENCES exams.publication(id),
    result_id            uuid         NOT NULL REFERENCES exams.term_result(id),
    registration_id      uuid         NOT NULL REFERENCES exams.exam_registration(id),
    outcome              text         NOT NULL CHECK (outcome IN ('published', 'revised', 'skipped')),
    detail               text,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (publication_id, result_id)
);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.publication_item FROM sutra_exams;
CREATE TRIGGER publication_item_append_only BEFORE UPDATE OR DELETE ON exams.publication_item
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ grade cards
CREATE TABLE exams.grade_card (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL REFERENCES exams.exam_session(id),
    registration_id      uuid         NOT NULL REFERENCES exams.exam_registration(id),
    result_id            uuid         NOT NULL REFERENCES exams.term_result(id),
    student_id           uuid         NOT NULL,
    subject_ref          text,
    card_number          text         NOT NULL,
    status               text         NOT NULL DEFAULT 'issued' CHECK (status IN ('issued', 'reissued', 'withdrawn')),
    status_reason        text,
    supersedes_id        uuid         REFERENCES exams.grade_card(id),
    issued_by            text,
    issued_at            timestamptz  NOT NULL DEFAULT now(),
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, card_number)
);
-- One valid card per candidate and session.
CREATE UNIQUE INDEX grade_card_one_valid ON exams.grade_card (registration_id)
    WHERE status <> 'withdrawn';
CREATE TRIGGER grade_card_no_delete BEFORE DELETE ON exams.grade_card
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ revaluation / rechecking (L2-I11)
CREATE TABLE exams.revaluation_request (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL REFERENCES exams.exam_session(id),
    registration_id      uuid         NOT NULL REFERENCES exams.exam_registration(id),
    paper_id             uuid         NOT NULL REFERENCES exams.exam_paper(id),
    paper_mark_id        uuid         NOT NULL REFERENCES exams.paper_mark(id),
    result_id            uuid         NOT NULL REFERENCES exams.term_result(id),
    student_id           uuid         NOT NULL,
    subject_ref          text,
    student_number       text         NOT NULL,
    display_name         text         NOT NULL,
    paper_code           text         NOT NULL,
    request_number       text         NOT NULL,
    kind                 text         NOT NULL CHECK (kind IN ('revaluation', 'rechecking')),
    status               text         NOT NULL DEFAULT 'submitted' CHECK (status IN (
                             'submitted', 'eligibility_verified', 'fee_pending', 'fee_verified',
                             'approved', 'review_in_progress', 'review_completed', 'outcome_verified',
                             'revised_marks_approved', 'result_revision_initiated',
                             'correction_review', 'withdrawn', 'rejected', 'closed', 'archived')),
    status_reason        text,
    reason               text,
    fee_amount           numeric(12,2) NOT NULL,
    fee_charge_id        uuid,
    fee_charge_number    text,
    from_marks           numeric(7,2),
    to_marks             numeric(7,2),
    review_notes         text,
    revised_result_id    uuid         REFERENCES exams.term_result(id),
    process_instance_id  text,
    requested_by         text,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, request_number)
);
-- One live review per candidate paper (L3-I-128/129 duplicates, L3-I-130 conflicting kinds).
CREATE UNIQUE INDEX revaluation_request_one_live ON exams.revaluation_request (registration_id, paper_id)
    WHERE status NOT IN ('withdrawn', 'rejected', 'closed', 'archived');
CREATE INDEX revaluation_request_session_idx ON exams.revaluation_request (tenant_id, session_id, status);
CREATE INDEX revaluation_request_subject_idx ON exams.revaluation_request (tenant_id, subject_ref);

-- ------------------------------------------------------------------ supplementary eligibility (L2-I12)
CREATE TABLE exams.supplementary_eligibility (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    result_id                uuid         NOT NULL REFERENCES exams.term_result(id),
    paper_id                 uuid         NOT NULL REFERENCES exams.exam_paper(id),
    curriculum_component_id  uuid         NOT NULL,
    student_id               uuid         NOT NULL,
    student_number           text         NOT NULL,
    display_name             text         NOT NULL,
    paper_code               text         NOT NULL,
    attempts_used            integer      NOT NULL CHECK (attempts_used >= 1),
    max_attempts             integer      NOT NULL CHECK (max_attempts >= 1),
    best_percent             numeric(5,2),
    best_grade               text,
    status                   text         NOT NULL CHECK (status IN ('eligible', 'applied', 'authorised', 'exhausted', 'cancelled')),
    status_reason            text,
    authorised_session_id    uuid         REFERENCES exams.exam_session(id),
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
-- One open opportunity per student and curriculum component (L3-I-138 duplicate attempts).
CREATE UNIQUE INDEX supplementary_eligibility_one_open ON exams.supplementary_eligibility
    (tenant_id, student_id, curriculum_component_id) WHERE status IN ('eligible', 'applied', 'authorised');
CREATE INDEX supplementary_eligibility_session_idx ON exams.supplementary_eligibility (tenant_id, session_id);

-- ------------------------------------------------------------------ completion evaluation (L2-I13)
CREATE TABLE exams.completion_evaluation (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    student_id               uuid         NOT NULL,
    student_context_id       uuid         NOT NULL,
    student_number           text         NOT NULL,
    display_name             text         NOT NULL,
    curriculum_version_id    uuid,
    credits_required         numeric(6,2) NOT NULL,
    credits_earned           numeric(6,2) NOT NULL,
    components_required      integer      NOT NULL,
    components_passed        integer      NOT NULL,
    cgpa                     numeric(4,2),
    status                   text         NOT NULL CHECK (status IN ('pending', 'eligible', 'not_eligible', 'blocked', 'superseded')),
    status_reason            text,
    -- Completion evidence record (L3-I-153): the published results and figures it rests on.
    evidence                 jsonb        NOT NULL DEFAULT '{}'::jsonb,
    supersedes_id            uuid         REFERENCES exams.completion_evaluation(id),
    evaluated_by             text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX completion_evaluation_one_current ON exams.completion_evaluation (tenant_id, student_id, institution_id)
    WHERE status <> 'superseded';
CREATE TRIGGER completion_evaluation_no_delete BEFORE DELETE ON exams.completion_evaluation
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ session closure (L2-I16)
CREATE TABLE exams.session_closure (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    session_id           uuid         NOT NULL UNIQUE REFERENCES exams.exam_session(id),
    status               text         NOT NULL DEFAULT 'closing' CHECK (status IN ('closing', 'closed', 'archived', 'reopened')),
    status_reason        text,
    checks               jsonb        NOT NULL DEFAULT '{}'::jsonb,
    completed_registrations integer   NOT NULL DEFAULT 0,
    process_instance_id  text,
    initiated_by         text,
    closed_at            timestamptz,
    archived_at          timestamptz,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
