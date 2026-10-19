-- Batch 6 slice 6B — malpractice basics (L2-I14; L3-I-162…168, 170, 171).
--
--   malpractice_case       one authoritative case per candidate (registration) and paper while open
--                          (duplicates → 409 / merge, L3-I-168); `MAL-` numbers. 6C reads decided
--                          cases READ-ONLY: the columns session_id, registration_id, paper_id,
--                          status, penalty, marks_deduction are a stable contract.
--   malpractice_decision   append-only record of every committee decision and review outcome.

CREATE TABLE exams.malpractice_case (
    id                     uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    session_id             uuid          NOT NULL REFERENCES exams.exam_session(id),
    registration_id        uuid          NOT NULL REFERENCES exams.exam_registration(id),
    paper_id               uuid          REFERENCES exams.exam_paper(id),
    student_id             uuid          NOT NULL,
    case_number            text          NOT NULL,
    allegation             text          NOT NULL CHECK (length(btrim(allegation)) BETWEEN 1 AND 2000),
    evidence               text,
    status                 text          NOT NULL DEFAULT 'reported' CHECK (status IN (
                               'reported', 'under_investigation', 'hearing', 'decided', 'appealed',
                               'under_review', 'upheld', 'modified', 'overturned', 'closed',
                               'archived')),
    status_reason          text,
    -- Effective penalty (6C reads it when status is decided | upheld | modified).
    penalty                text          CHECK (penalty IS NULL OR penalty IN (
                               'none', 'warning', 'marks_deduction', 'paper_cancelled',
                               'session_cancelled', 'debarred')),
    marks_deduction        numeric(7,2)  CHECK (marks_deduction IS NULL OR marks_deduction >= 0),
    -- Proposal awaiting the governed decision (WorkflowPort AwaitDecision).
    proposal_kind          text          CHECK (proposal_kind IS NULL OR proposal_kind IN ('decision', 'review')),
    proposed_outcome       text          CHECK (proposed_outcome IS NULL OR proposed_outcome IN (
                               'decided', 'upheld', 'modified', 'overturned')),
    proposed_penalty       text,
    proposed_deduction     numeric(7,2),
    proposed_reason        text,
    -- Registered paper statuses before a debarment, restored when the finding is overturned.
    prior_paper_statuses   jsonb         NOT NULL DEFAULT '{}'::jsonb,
    merged_into_id         uuid          REFERENCES exams.malpractice_case(id),
    process_instance_id    text,
    reported_by            text,
    decided_at             timestamptz,
    closed_at              timestamptz,
    archived_at            timestamptz,
    version                bigint        NOT NULL DEFAULT 1,
    created_at             timestamptz   NOT NULL DEFAULT now(),
    updated_at             timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, case_number)
);
CREATE INDEX malpractice_case_session_idx ON exams.malpractice_case (tenant_id, session_id);
-- One open case per candidate and paper (NULL paper = the whole registration).
CREATE UNIQUE INDEX malpractice_case_open_uq ON exams.malpractice_case
    (tenant_id, registration_id, COALESCE(paper_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE status NOT IN ('closed', 'archived') AND merged_into_id IS NULL;

CREATE TABLE exams.malpractice_decision (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    case_id              uuid          NOT NULL REFERENCES exams.malpractice_case(id),
    kind                 text          NOT NULL CHECK (kind IN ('decision', 'review', 'merge', 'reopen')),
    outcome              text          NOT NULL,
    penalty              text,
    marks_deduction      numeric(7,2),
    reason               text,
    decided_by           text,
    process_instance_id  text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX malpractice_decision_case_idx ON exams.malpractice_decision (tenant_id, case_id);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.malpractice_decision FROM sutra_exams;
CREATE TRIGGER malpractice_decision_append_only BEFORE UPDATE OR DELETE ON exams.malpractice_decision
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();
