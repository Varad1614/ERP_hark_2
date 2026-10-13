-- Batch 6 slice 6B — assessment & marks (docs/batch6-contracts.md §3.6).
--
--   mark_sheet              one per paper: the per-paper evaluation lifecycle (L2-I08 §20) and the
--                           final marks approval (MARKS_APPROVAL, the reference approval of Batch 6).
--                           Candidate values live in the foundation `paper_mark` rows and change
--                           ONLY through `shared::marks::update`.
--   evaluation_assignment   evaluators of a sheet (examiner, panel member, second examiner,
--                           adjudicator) — login subjects (no People record until Batch 7).
--   evaluation              one evaluation of one candidate's script (first / second / adjudication);
--                           panel members each record a first-round evaluation (L3-I-073, 080…082).
--   moderation              policy moderation of a paper (grace / scaling, MODERATION_APPROVAL) with
--                           its before / after report (L3-I-075).
--   marks_exception         marks submission exceptions (late / missing marks, L3-I-074).
--
-- Decimal rules: marks numeric(7,2); percentages numeric(5,2).

CREATE TABLE exams.mark_sheet (
    id                          uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                   uuid          NOT NULL,
    institution_id              uuid          NOT NULL,
    session_id                  uuid          NOT NULL REFERENCES exams.exam_session(id),
    paper_id                    uuid          NOT NULL REFERENCES exams.exam_paper(id),
    status                      text          NOT NULL DEFAULT 'planned' CHECK (status IN (
                                    'pending', 'planned', 'assigned', 'in_evaluation', 'evaluated',
                                    'moderation', 'validated', 'approved', 'finalised', 'closed',
                                    'archived')),
    status_reason               text,
    -- Submission deadline (business date); a later submission needs an accepted exception.
    due_on                      date,
    -- Policy: every gradable script needs a second evaluation; a first / second difference
    -- above the threshold needs an adjudicating evaluation.
    second_evaluation_required  boolean       NOT NULL DEFAULT false,
    difference_threshold        numeric(7,2)  NOT NULL DEFAULT 10 CHECK (difference_threshold >= 0),
    submitted_by                text,
    submitted_at                timestamptz,
    verified_by                 text,
    verified_at                 timestamptz,
    approval_requested_at       timestamptz,
    approved_by                 text,
    approved_at                 timestamptz,
    finalised_at                timestamptz,
    process_instance_id         text,
    version                     bigint        NOT NULL DEFAULT 1,
    created_by                  text,
    created_at                  timestamptz   NOT NULL DEFAULT now(),
    updated_at                  timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, paper_id)
);
CREATE INDEX mark_sheet_session_idx ON exams.mark_sheet (tenant_id, session_id);

CREATE TABLE exams.evaluation_assignment (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    mark_sheet_id    uuid         NOT NULL REFERENCES exams.mark_sheet(id),
    evaluator_ref    text         NOT NULL CHECK (length(btrim(evaluator_ref)) BETWEEN 1 AND 200),
    evaluator_name   text,
    role             text         NOT NULL CHECK (role IN ('examiner', 'panel_member', 'second_examiner', 'adjudicator')),
    status           text         NOT NULL DEFAULT 'assigned' CHECK (status IN ('assigned', 'released')),
    version          bigint       NOT NULL DEFAULT 1,
    created_by       text,
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX evaluation_assignment_live_uq ON exams.evaluation_assignment
    (tenant_id, mark_sheet_id, evaluator_ref, role) WHERE status = 'assigned';

CREATE TABLE exams.evaluation (
    id               uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid          NOT NULL,
    institution_id   uuid          NOT NULL,
    session_id       uuid          NOT NULL REFERENCES exams.exam_session(id),
    paper_id         uuid          NOT NULL REFERENCES exams.exam_paper(id),
    mark_sheet_id    uuid          NOT NULL REFERENCES exams.mark_sheet(id),
    paper_mark_id    uuid          NOT NULL REFERENCES exams.paper_mark(id),
    registration_id  uuid          NOT NULL REFERENCES exams.exam_registration(id),
    round            text          NOT NULL CHECK (round IN ('first', 'second', 'adjudication')),
    evaluator_ref    text          NOT NULL,
    marks            numeric(7,2)  NOT NULL CHECK (marks >= 0),
    remark           text,
    status           text          NOT NULL DEFAULT 'recorded' CHECK (status IN ('recorded', 'cancelled')),
    created_at       timestamptz   NOT NULL DEFAULT now()
);
-- One evaluation per evaluator and round; one second / adjudicating evaluation per script.
CREATE UNIQUE INDEX evaluation_evaluator_uq ON exams.evaluation (tenant_id, paper_mark_id, round, evaluator_ref)
    WHERE status = 'recorded';
CREATE UNIQUE INDEX evaluation_single_round_uq ON exams.evaluation (tenant_id, paper_mark_id, round)
    WHERE status = 'recorded' AND round IN ('second', 'adjudication');
CREATE INDEX evaluation_paper_idx ON exams.evaluation (tenant_id, paper_id);

CREATE TABLE exams.moderation (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    session_id           uuid          NOT NULL REFERENCES exams.exam_session(id),
    paper_id             uuid          NOT NULL REFERENCES exams.exam_paper(id),
    mark_sheet_id        uuid          NOT NULL REFERENCES exams.mark_sheet(id),
    kind                 text          NOT NULL CHECK (kind IN ('grace', 'scaling')),
    -- grace: marks added to every verified script; scaling: percentage factor on external marks.
    value                numeric(7,2)  NOT NULL CHECK (value > 0),
    reason               text          NOT NULL,
    status               text          NOT NULL DEFAULT 'requested' CHECK (status IN (
                             'requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    status_reason        text,
    report               jsonb         NOT NULL DEFAULT '[]'::jsonb,
    applied_count        integer       NOT NULL DEFAULT 0,
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    applied_at           timestamptz,
    version              bigint        NOT NULL DEFAULT 1,
    created_by           text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
-- At most one open moderation per paper.
CREATE UNIQUE INDEX moderation_open_uq ON exams.moderation (tenant_id, paper_id)
    WHERE status IN ('requested', 'validated', 'approved');

CREATE TABLE exams.marks_exception (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    session_id       uuid         NOT NULL REFERENCES exams.exam_session(id),
    paper_id         uuid         NOT NULL REFERENCES exams.exam_paper(id),
    mark_sheet_id    uuid         NOT NULL REFERENCES exams.mark_sheet(id),
    paper_mark_id    uuid         REFERENCES exams.paper_mark(id),
    kind             text         NOT NULL CHECK (kind IN ('late_submission', 'missing_marks')),
    reason           text         NOT NULL,
    status           text         NOT NULL DEFAULT 'raised' CHECK (status IN ('raised', 'accepted', 'rejected')),
    resolution       text,
    resolved_by      text,
    resolved_at      timestamptz,
    version          bigint       NOT NULL DEFAULT 1,
    created_by       text,
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX marks_exception_sheet_idx ON exams.marks_exception (tenant_id, mark_sheet_id);
