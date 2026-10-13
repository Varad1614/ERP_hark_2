-- Batch 6 foundation — Domain I (Examinations) shared tables (docs/batch6-contracts.md §3).
--
-- Foundation-owned. Slices 6A/6B/6C add their own tables in their migration ranges
-- (6A 20261105000010-029, 6B …030-049, 6C …050-069) and never edit this file.
--
-- Marks, credits, percentages and grade points are NUMERIC (rust_decimal in Rust — never floats);
-- on the wire they are JSON strings. Domain B/C/D/F ids are plain uuids (no cross-schema FKs).
--
-- Write rules (§3.3):
--   grading_scheme / grade_band   foundation API (`grading/`), insert + retire only.
--   exam_session                  6A creates / edits; status ONLY via
--                                 `shared::lifecycle::change_session_status` (6A draft … reopened,
--                                 6C closing → closed → archived).
--   exam_paper                    6A creates / edits; status ONLY via `change_paper_status`
--                                 (6A up to `conducted` / `cancelled`, 6B `evaluation` / `marks_finalised`).
--   exam_registration(_paper)     6A; registration status ONLY via `change_registration_status`;
--                                 rows inserted ONLY via `shared::registration::insert_registration`
--                                 (which also creates the `paper_mark` rows).
--   paper_mark                    6B through `shared::marks::{record, change_mark_status}`;
--                                 6C changes final marks ONLY via `shared::marks::revise_final`
--                                 (revaluation, approved correction). Every change appends
--                                 `paper_mark_history` (append-only).
--   term_result(_line)            6C ONLY via `shared::results::{insert_result, change_result_status}`
--                                 (lines are immutable; a recompute / revision inserts a new result
--                                 row that supersedes the old one).
--   number_sequence               `shared::numbering::next` only.
--   status_history                `shared::lifecycle` only (append-only).

-- Append-only guard reused by every exams history table (slices reuse it too).
CREATE FUNCTION exams.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of exams.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- ------------------------------------------------------------------ grading schemes (foundation API)
CREATE TABLE exams.grading_scheme (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    status               text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'retired')),
    status_reason        text,
    version              bigint       NOT NULL DEFAULT 1,
    created_by           text,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE exams.grade_band (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    scheme_id            uuid          NOT NULL REFERENCES exams.grading_scheme(id),
    grade                text          NOT NULL CHECK (grade ~ '^[A-Z][A-Z+-]{0,3}$'),
    min_percent          numeric(5,2)  NOT NULL CHECK (min_percent >= 0 AND min_percent <= 100),
    grade_point          numeric(4,2)  NOT NULL CHECK (grade_point >= 0 AND grade_point <= 10),
    is_pass              boolean       NOT NULL,
    UNIQUE (scheme_id, grade),
    UNIQUE (scheme_id, min_percent)
);
CREATE TRIGGER grade_band_immutable BEFORE UPDATE OR DELETE ON exams.grade_band
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ examination session (6A / 6C)
CREATE TABLE exams.exam_session (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    academic_year_id         uuid,
    academic_term_id         uuid         NOT NULL,
    code                     text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                     text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    kind                     text         NOT NULL DEFAULT 'regular' CHECK (kind IN (
                                 'regular', 'supplementary', 'improvement', 'makeup', 'special')),
    -- 6A's governing policy version (plain: 6A table).
    policy_id                uuid,
    grading_scheme_id        uuid         REFERENCES exams.grading_scheme(id),
    starts_on                date,
    ends_on                  date,
    registration_opens_on    date,
    registration_closes_on   date,
    status                   text         NOT NULL DEFAULT 'draft' CHECK (status IN (
                                 'draft', 'under_review', 'approved', 'scheduled', 'active',
                                 'suspended', 'reopened', 'closing', 'closed', 'archived')),
    status_reason            text,
    status_changed_at        timestamptz,
    process_instance_id      text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code),
    CHECK (ends_on IS NULL OR starts_on IS NULL OR ends_on >= starts_on),
    CHECK (registration_closes_on IS NULL OR registration_opens_on IS NULL
           OR registration_closes_on >= registration_opens_on)
);
CREATE INDEX exam_session_term_idx ON exams.exam_session (tenant_id, institution_id, academic_term_id);
CREATE TRIGGER exam_session_no_delete BEFORE DELETE ON exams.exam_session
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ examinable paper (6A / 6B)
CREATE TABLE exams.exam_paper (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    institution_id           uuid          NOT NULL,
    session_id               uuid          NOT NULL REFERENCES exams.exam_session(id),
    -- Domain D course offering / Domain B curriculum component (plain ids).
    offering_id              uuid,
    curriculum_component_id  uuid          NOT NULL,
    academic_term_id         uuid          NOT NULL,
    paper_code               text          NOT NULL CHECK (paper_code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    title                    text          NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    credits                  numeric(5,2)  NOT NULL CHECK (credits > 0 AND credits <= 100),
    mode                     text          NOT NULL DEFAULT 'theory' CHECK (mode IN ('theory', 'practical', 'viva', 'project')),
    max_marks                numeric(7,2)  NOT NULL CHECK (max_marks > 0),
    pass_percent             numeric(5,2)  NOT NULL DEFAULT 40 CHECK (pass_percent >= 0 AND pass_percent <= 100),
    internal_weight          numeric(5,2)  NOT NULL DEFAULT 0 CHECK (internal_weight >= 0 AND internal_weight <= 100),
    duration_minutes         integer       CHECK (duration_minutes IS NULL OR duration_minutes BETWEEN 1 AND 1440),
    status                   text          NOT NULL DEFAULT 'draft' CHECK (status IN (
                                 'draft', 'scheduled', 'ready', 'conducted', 'evaluation',
                                 'marks_finalised', 'cancelled')),
    status_reason            text,
    version                  bigint        NOT NULL DEFAULT 1,
    created_at               timestamptz   NOT NULL DEFAULT now(),
    updated_at               timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (session_id, paper_code)
);
CREATE UNIQUE INDEX exam_paper_one_per_offering ON exams.exam_paper (session_id, offering_id)
    WHERE offering_id IS NOT NULL AND status <> 'cancelled';
CREATE INDEX exam_paper_session_idx ON exams.exam_paper (tenant_id, session_id);

-- ------------------------------------------------------------------ registration (6A)
CREATE TABLE exams.exam_registration (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    -- Domain C student + StudentContext (plain ids) and snapshots (no join into `student`).
    student_id               uuid         NOT NULL,
    student_context_id       uuid         NOT NULL,
    registration_number      text         NOT NULL,
    student_number           text         NOT NULL,
    display_name             text         NOT NULL,
    subject_ref              text,
    channel                  text         NOT NULL DEFAULT 'self_service' CHECK (channel IN (
                                 'self_service', 'bulk', 'administrative', 'late')),
    status                   text         NOT NULL DEFAULT 'draft' CHECK (status IN (
                                 'draft', 'pending_validation', 'pending_review', 'approved',
                                 'confirmed', 'locked', 'amended', 'withdrawn', 'rejected',
                                 'completed', 'archived')),
    status_reason            text,
    status_changed_at        timestamptz,
    process_instance_id      text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, registration_number)
);
-- One live registration per candidate and session (L3-I-032 duplicates collapse onto it).
CREATE UNIQUE INDEX exam_registration_one_live ON exams.exam_registration (session_id, student_id)
    WHERE status NOT IN ('withdrawn', 'rejected', 'archived');
CREATE INDEX exam_registration_subject_idx ON exams.exam_registration (tenant_id, subject_ref);
CREATE TRIGGER exam_registration_no_delete BEFORE DELETE ON exams.exam_registration
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.exam_registration_paper (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    paper_id                 uuid         NOT NULL REFERENCES exams.exam_paper(id),
    attempt_kind             text         NOT NULL DEFAULT 'regular' CHECK (attempt_kind IN (
                                 'regular', 'backlog', 'supplementary', 'improvement')),
    attempt_no               integer      NOT NULL DEFAULT 1 CHECK (attempt_no BETWEEN 1 AND 20),
    status                   text         NOT NULL DEFAULT 'registered' CHECK (status IN (
                                 'registered', 'withdrawn', 'absent', 'appeared', 'debarred')),
    status_reason            text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (registration_id, paper_id)
);
CREATE INDEX exam_registration_paper_paper_idx ON exams.exam_registration_paper (paper_id);
CREATE TRIGGER exam_registration_paper_no_delete BEFORE DELETE ON exams.exam_registration_paper
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ marks (6B; 6C revise_final)
CREATE TABLE exams.paper_mark (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    institution_id           uuid          NOT NULL,
    session_id               uuid          NOT NULL REFERENCES exams.exam_session(id),
    paper_id                 uuid          NOT NULL REFERENCES exams.exam_paper(id),
    registration_id          uuid          NOT NULL REFERENCES exams.exam_registration(id),
    registration_paper_id    uuid          NOT NULL UNIQUE REFERENCES exams.exam_registration_paper(id),
    student_id               uuid          NOT NULL,
    internal_marks           numeric(7,2)  CHECK (internal_marks IS NULL OR internal_marks >= 0),
    internal_max             numeric(7,2)  CHECK (internal_max IS NULL OR internal_max > 0),
    -- Domain D internal assessment result the internal marks came from (plain id).
    internal_source_id       uuid,
    external_marks           numeric(7,2)  CHECK (external_marks IS NULL OR external_marks >= 0),
    external_max             numeric(7,2)  NOT NULL CHECK (external_max > 0),
    grace_marks              numeric(7,2)  NOT NULL DEFAULT 0 CHECK (grace_marks >= 0),
    absent                   boolean       NOT NULL DEFAULT false,
    status                   text          NOT NULL DEFAULT 'pending' CHECK (status IN (
                                 'pending', 'entered', 'submitted', 'verified', 'moderated',
                                 'approved', 'withheld', 'finalised')),
    status_reason            text,
    entered_by               text,
    finalised_at             timestamptz,
    revision                 integer       NOT NULL DEFAULT 0,
    version                  bigint        NOT NULL DEFAULT 1,
    created_at               timestamptz   NOT NULL DEFAULT now(),
    updated_at               timestamptz   NOT NULL DEFAULT now(),
    CHECK (internal_marks IS NULL OR internal_max IS NOT NULL AND internal_marks <= internal_max),
    CHECK (external_marks IS NULL OR external_marks <= external_max)
);
CREATE INDEX paper_mark_paper_idx ON exams.paper_mark (tenant_id, paper_id);
CREATE INDEX paper_mark_registration_idx ON exams.paper_mark (registration_id);
CREATE TRIGGER paper_mark_no_delete BEFORE DELETE ON exams.paper_mark
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.paper_mark_history (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    paper_mark_id            uuid          NOT NULL REFERENCES exams.paper_mark(id),
    from_status              text,
    to_status                text          NOT NULL,
    internal_marks           numeric(7,2),
    external_marks           numeric(7,2),
    grace_marks              numeric(7,2)  NOT NULL DEFAULT 0,
    absent                   boolean       NOT NULL DEFAULT false,
    -- `entry`, `submit`, `verify`, `moderation`, `approve`, `withhold`, `finalise`, `revaluation`, `correction`, …
    cause                    text          NOT NULL,
    reason                   text,
    actor                    text,
    correlation_id           text,
    created_at               timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX paper_mark_history_mark_idx ON exams.paper_mark_history (paper_mark_id, created_at);
CREATE TRIGGER paper_mark_history_append_only BEFORE UPDATE OR DELETE ON exams.paper_mark_history
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ term results (6C)
CREATE TABLE exams.term_result (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    institution_id           uuid          NOT NULL,
    session_id               uuid          NOT NULL REFERENCES exams.exam_session(id),
    registration_id          uuid          NOT NULL REFERENCES exams.exam_registration(id),
    student_id               uuid          NOT NULL,
    student_context_id       uuid          NOT NULL,
    academic_term_id         uuid          NOT NULL,
    student_number           text          NOT NULL,
    display_name             text          NOT NULL,
    subject_ref              text,
    grading_scheme_id        uuid          NOT NULL REFERENCES exams.grading_scheme(id),
    credits_attempted        numeric(6,2)  NOT NULL DEFAULT 0 CHECK (credits_attempted >= 0),
    credits_earned           numeric(6,2)  NOT NULL DEFAULT 0 CHECK (credits_earned >= 0),
    -- Σ credits × grade point over the graded lines.
    grade_points             numeric(8,2)  NOT NULL DEFAULT 0 CHECK (grade_points >= 0),
    sgpa                     numeric(4,2)  CHECK (sgpa IS NULL OR sgpa BETWEEN 0 AND 10),
    cgpa                     numeric(4,2)  CHECK (cgpa IS NULL OR cgpa BETWEEN 0 AND 10),
    outcome                  text          NOT NULL CHECK (outcome IN ('pass', 'fail', 'withheld', 'incomplete', 'malpractice')),
    status                   text          NOT NULL DEFAULT 'computed' CHECK (status IN (
                                 'pending', 'computed', 'validated', 'approved', 'withheld',
                                 'published', 'withdrawn', 'superseded', 'archived')),
    status_reason            text,
    revision                 integer       NOT NULL DEFAULT 1 CHECK (revision >= 1),
    supersedes_id            uuid          REFERENCES exams.term_result(id),
    -- 6C result batch that computed this row (plain: 6C table).
    result_batch_id          uuid,
    process_instance_id      text,
    published_at             timestamptz,
    version                  bigint        NOT NULL DEFAULT 1,
    created_at               timestamptz   NOT NULL DEFAULT now(),
    updated_at               timestamptz   NOT NULL DEFAULT now(),
    CHECK (credits_earned <= credits_attempted)
);
-- One current result per candidate and session (L3-I-093 duplicates collapse onto it).
CREATE UNIQUE INDEX term_result_one_current ON exams.term_result (session_id, student_id)
    WHERE status NOT IN ('superseded', 'archived');
CREATE INDEX term_result_student_term_idx ON exams.term_result (tenant_id, student_id, academic_term_id);
CREATE INDEX term_result_subject_idx ON exams.term_result (tenant_id, subject_ref);
CREATE TRIGGER term_result_no_delete BEFORE DELETE ON exams.term_result
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.term_result_line (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    result_id                uuid          NOT NULL REFERENCES exams.term_result(id),
    line_no                  integer       NOT NULL CHECK (line_no >= 1),
    paper_id                 uuid          NOT NULL REFERENCES exams.exam_paper(id),
    curriculum_component_id  uuid          NOT NULL,
    paper_code               text          NOT NULL,
    title                    text          NOT NULL,
    credits                  numeric(5,2)  NOT NULL CHECK (credits > 0),
    attempt_kind             text          NOT NULL DEFAULT 'regular',
    internal_marks           numeric(7,2),
    external_marks           numeric(7,2),
    grace_marks              numeric(7,2)  NOT NULL DEFAULT 0,
    total_marks              numeric(7,2),
    max_marks                numeric(7,2)  NOT NULL CHECK (max_marks > 0),
    percent                  numeric(5,2)  CHECK (percent IS NULL OR percent BETWEEN 0 AND 100),
    -- NULL grade = not graded (absent / withheld / incomplete).
    grade                    text,
    grade_point              numeric(4,2)  CHECK (grade_point IS NULL OR grade_point BETWEEN 0 AND 10),
    passed                   boolean       NOT NULL DEFAULT false,
    remark                   text,
    UNIQUE (result_id, line_no),
    UNIQUE (result_id, paper_id)
);
CREATE TRIGGER term_result_line_immutable BEFORE UPDATE OR DELETE ON exams.term_result_line
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ numbering + status history
CREATE TABLE exams.number_sequence (
    tenant_id        uuid    NOT NULL,
    institution_id   uuid    NOT NULL,
    kind             text    NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,47}$'),
    next_value       bigint  NOT NULL CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

CREATE TABLE exams.status_history (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    aggregate_type       text         NOT NULL,
    aggregate_id         uuid         NOT NULL,
    from_status          text,
    to_status            text         NOT NULL,
    reason               text,
    cause_event_type     text,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX exams_status_history_aggregate_idx
    ON exams.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER exams_status_history_append_only BEFORE UPDATE OR DELETE ON exams.status_history
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- ------------------------------------------------------------------ runtime role (sutra_exams)
-- The role and the schema default privileges come from 20261014000003_runtime_db_roles.sql; the
-- grants are restated here so the foundation tables are covered on clusters provisioned out of
-- band, and the history / line tables are append-only at the privilege level as well.
GRANT USAGE ON SCHEMA exams TO sutra_exams;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA exams TO sutra_exams;
REVOKE UPDATE, DELETE, TRUNCATE ON exams.paper_mark_history, exams.status_history,
    exams.term_result_line, exams.grade_band FROM sutra_exams;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_exams;
