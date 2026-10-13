-- 4B — Internal assessment and coursework (L2-D07, L3-D-051…057).
--
-- Blueprint (components + weightage + policy) → assessment windows (coursework = kind
-- `assignment`) → one submission per student and window → moderation (new governed fact, the
-- original marks stay) → internal assessment result per student (finalised; a correction marks the
-- original `corrected` and inserts the next `result_version`) → archive.

CREATE TABLE academic.assessment_blueprint (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    offering_id     uuid         NOT NULL REFERENCES academic.course_offering(id),
    section_id      uuid         NOT NULL REFERENCES academic.offering_section(id),
    title           text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    total_marks     numeric(7,2) NOT NULL CHECK (total_marks > 0),
    -- [{"key","title","kind","weight","maxMarks"}]; weights add up to 100 at publication.
    components      jsonb        NOT NULL DEFAULT '[]'::jsonb,
    -- {"moderationRequired": bool, "allowLateSubmission": bool}.
    policy          jsonb        NOT NULL DEFAULT '{}'::jsonb,
    status          text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'superseded', 'archived')),
    status_reason   text,
    supersedes_id   uuid         REFERENCES academic.assessment_blueprint(id),
    published_at    timestamptz,
    published_by    text,
    version         bigint       NOT NULL DEFAULT 1,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX assessment_blueprint_one_published ON academic.assessment_blueprint (section_id)
    WHERE status = 'published';
CREATE INDEX assessment_blueprint_section ON academic.assessment_blueprint (tenant_id, section_id);

CREATE TABLE academic.assessment_window (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    offering_id      uuid         NOT NULL REFERENCES academic.course_offering(id),
    section_id       uuid         NOT NULL REFERENCES academic.offering_section(id),
    blueprint_id     uuid         NOT NULL REFERENCES academic.assessment_blueprint(id),
    component_key    text         NOT NULL,
    title            text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    kind             text         NOT NULL CHECK (kind IN ('assignment', 'quiz', 'test', 'lab', 'project', 'presentation', 'viva')),
    instructions     text,
    opens_at         timestamptz  NOT NULL,
    due_at           timestamptz  NOT NULL,
    extended_due_at  timestamptz,
    max_marks        numeric(7,2) NOT NULL CHECK (max_marks > 0),
    status           text         NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'open', 'extended', 'closed', 'cancelled')),
    status_reason    text,
    created_by       text,
    version          bigint       NOT NULL DEFAULT 1,
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now(),
    CHECK (due_at > opens_at),
    CHECK (extended_due_at IS NULL OR extended_due_at > due_at)
);
CREATE INDEX assessment_window_section ON academic.assessment_window (tenant_id, section_id, due_at);

CREATE TABLE academic.assessment_submission (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    window_id       uuid         NOT NULL REFERENCES academic.assessment_window(id),
    section_id      uuid         NOT NULL REFERENCES academic.offering_section(id),
    student_id      uuid         NOT NULL,
    student_number  text         NOT NULL,
    display_name    text         NOT NULL,
    subject_ref     text,
    status          text         NOT NULL CHECK (status IN ('pending', 'submitted', 'late', 'marked', 'moderated', 'finalized', 'absent')),
    content         text,
    attachment_url  text,
    submitted_at    timestamptz,
    marks           numeric(7,2) CHECK (marks IS NULL OR marks >= 0),
    feedback        text,
    marked_by       text,
    marked_at       timestamptz,
    moderated_marks numeric(7,2) CHECK (moderated_marks IS NULL OR moderated_marks >= 0),
    version         bigint       NOT NULL DEFAULT 1,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    -- One governed submission per student and assessment opportunity (L4-D-053-BH-004).
    UNIQUE (window_id, student_id)
);
CREATE INDEX assessment_submission_student ON academic.assessment_submission (tenant_id, student_id);
CREATE TRIGGER assessment_submission_no_delete BEFORE DELETE ON academic.assessment_submission
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Every mark recorded or changed before moderation (append-only, L4-D-053-BH-005).
CREATE TABLE academic.assessment_mark_history (
    id             uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid         NOT NULL,
    submission_id  uuid         NOT NULL REFERENCES academic.assessment_submission(id),
    from_marks     numeric(7,2),
    to_marks       numeric(7,2),
    cause          text         NOT NULL CHECK (cause IN ('marked', 'remarked', 'absent', 'moderated')),
    reason         text,
    actor          text,
    recorded_at    timestamptz  NOT NULL DEFAULT now()
);
CREATE TRIGGER assessment_mark_history_immutable BEFORE UPDATE OR DELETE ON academic.assessment_mark_history
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.assessment_moderation (
    id                  uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid         NOT NULL,
    institution_id      uuid         NOT NULL,
    window_id           uuid         NOT NULL REFERENCES academic.assessment_window(id),
    submission_id       uuid         NOT NULL REFERENCES academic.assessment_submission(id),
    original_marks      numeric(7,2) NOT NULL,
    proposed_marks      numeric(7,2) NOT NULL CHECK (proposed_marks >= 0),
    reason              text         NOT NULL CHECK (length(btrim(reason)) > 0),
    evidence            text,
    status              text         NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    status_reason       text,
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    decided_at          timestamptz,
    version             bigint       NOT NULL DEFAULT 1,
    created_at          timestamptz  NOT NULL DEFAULT now(),
    updated_at          timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX assessment_moderation_one_open ON academic.assessment_moderation (submission_id)
    WHERE status IN ('requested', 'validated', 'approved');
CREATE INDEX assessment_moderation_window ON academic.assessment_moderation (window_id);

CREATE TABLE academic.internal_assessment_result (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    offering_id              uuid         NOT NULL REFERENCES academic.course_offering(id),
    section_id               uuid         NOT NULL REFERENCES academic.offering_section(id),
    blueprint_id             uuid         NOT NULL REFERENCES academic.assessment_blueprint(id),
    academic_term_id         uuid         NOT NULL,
    curriculum_component_id  uuid         NOT NULL,
    student_id               uuid         NOT NULL,
    student_number           text         NOT NULL,
    display_name             text         NOT NULL,
    subject_ref              text,
    -- [{"key","title","weight","obtained","max"}]
    component_marks          jsonb        NOT NULL DEFAULT '[]'::jsonb,
    marks_obtained           numeric(7,2) NOT NULL CHECK (marks_obtained >= 0),
    max_marks                numeric(7,2) NOT NULL CHECK (max_marks > 0),
    status                   text         NOT NULL CHECK (status IN ('in_progress', 'moderated', 'finalized', 'corrected', 'archived')),
    status_reason            text,
    result_version           integer      NOT NULL DEFAULT 1,
    supersedes_id            uuid         REFERENCES academic.internal_assessment_result(id),
    process_instance_id      text,
    finalized_at             timestamptz,
    finalized_by             text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX internal_assessment_result_current ON academic.internal_assessment_result (blueprint_id, student_id)
    WHERE status IN ('in_progress', 'moderated', 'finalized', 'archived');
CREATE INDEX internal_assessment_result_section ON academic.internal_assessment_result (tenant_id, section_id);
CREATE TRIGGER internal_assessment_result_no_delete BEFORE DELETE ON academic.internal_assessment_result
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.assessment_correction (
    id                  uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid         NOT NULL,
    institution_id      uuid         NOT NULL,
    result_id           uuid         NOT NULL REFERENCES academic.internal_assessment_result(id),
    corrected_result_id uuid         REFERENCES academic.internal_assessment_result(id),
    from_marks          numeric(7,2) NOT NULL,
    to_marks            numeric(7,2) NOT NULL CHECK (to_marks >= 0),
    reason              text         NOT NULL CHECK (length(btrim(reason)) > 0),
    evidence            text,
    status              text         NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    status_reason       text,
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    decided_at          timestamptz,
    version             bigint       NOT NULL DEFAULT 1,
    created_at          timestamptz  NOT NULL DEFAULT now(),
    updated_at          timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX assessment_correction_one_open ON academic.assessment_correction (result_id)
    WHERE status IN ('requested', 'validated', 'approved');
