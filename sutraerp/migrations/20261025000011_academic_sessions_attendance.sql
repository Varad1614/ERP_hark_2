-- 4B — Teaching sessions and attendance (L2-D04, L3-D-023…025; docs/batch4-contracts.md §3.3).
--
-- A teaching session of a section (optionally from a timetable slot) with one canonical outcome;
-- one attendance record per seated student and session (roster at capture time); governed
-- corrections leave the finalised fact in `attendance_audit` (append-only). Attendance policy
-- versions and the per-student threshold evaluation feed `attendance.threshold_breached`.

CREATE TABLE academic.teaching_session (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL,
    offering_id             uuid        NOT NULL REFERENCES academic.course_offering(id),
    section_id              uuid        NOT NULL REFERENCES academic.offering_section(id),
    timetable_slot_id       uuid        REFERENCES academic.timetable_slot(id),
    session_date            date        NOT NULL,
    starts_at               time        NOT NULL,
    ends_at                 time        NOT NULL,
    topic                   text,
    notes                   text,
    status                  text        NOT NULL DEFAULT 'scheduled' CHECK (status IN (
        'scheduled', 'started', 'in_progress', 'paused', 'completed', 'cancelled', 'abandoned',
        'locked', 'archived')),
    attendance_status       text        NOT NULL DEFAULT 'not_started' CHECK (attendance_status IN (
        'not_started', 'capturing', 'submitted', 'finalised')),
    status_reason           text,
    status_changed_at       timestamptz NOT NULL DEFAULT now(),
    interruption_count      integer     NOT NULL DEFAULT 0,
    started_at              timestamptz,
    ended_at                timestamptz,
    attendance_finalised_at timestamptz,
    attendance_finalised_by text,
    created_by              text,
    version                 bigint      NOT NULL DEFAULT 1,
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE UNIQUE INDEX teaching_session_one_per_time ON academic.teaching_session (section_id, session_date, starts_at)
    WHERE status NOT IN ('cancelled');
CREATE INDEX teaching_session_section ON academic.teaching_session (tenant_id, section_id, session_date);
CREATE TRIGGER teaching_session_no_delete BEFORE DELETE ON academic.teaching_session
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.attendance_record (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL,
    session_id      uuid        NOT NULL REFERENCES academic.teaching_session(id),
    section_id      uuid        NOT NULL REFERENCES academic.offering_section(id),
    student_id      uuid        NOT NULL,
    student_number  text        NOT NULL,
    display_name    text        NOT NULL,
    subject_ref     text,
    mark            text        NOT NULL CHECK (mark IN ('present', 'absent', 'late', 'excused')),
    remarks         text,
    recorded_by     text,
    version         bigint      NOT NULL DEFAULT 1,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    -- One canonical record per student and session (L4-D-024-BH-002).
    UNIQUE (session_id, student_id)
);
CREATE INDEX attendance_record_student ON academic.attendance_record (tenant_id, section_id, student_id);
CREATE TRIGGER attendance_record_no_delete BEFORE DELETE ON academic.attendance_record
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.attendance_correction (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    session_id          uuid        NOT NULL REFERENCES academic.teaching_session(id),
    record_id           uuid        NOT NULL REFERENCES academic.attendance_record(id),
    student_id          uuid        NOT NULL,
    from_mark           text        NOT NULL CHECK (from_mark IN ('present', 'absent', 'late', 'excused')),
    to_mark             text        NOT NULL CHECK (to_mark IN ('present', 'absent', 'late', 'excused')),
    reason              text        NOT NULL CHECK (length(btrim(reason)) > 0),
    evidence            text,
    status              text        NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    status_reason       text,
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    decided_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX attendance_correction_one_open ON academic.attendance_correction (record_id)
    WHERE status IN ('requested', 'validated', 'approved');
CREATE INDEX attendance_correction_session ON academic.attendance_correction (session_id);

-- Append-only history of every finalised mark change (the original fact is never lost).
CREATE TABLE academic.attendance_audit (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    record_id      uuid        NOT NULL REFERENCES academic.attendance_record(id),
    correction_id  uuid        REFERENCES academic.attendance_correction(id),
    from_mark      text        NOT NULL,
    to_mark        text        NOT NULL,
    actor          text,
    recorded_at    timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER attendance_audit_immutable BEFORE UPDATE OR DELETE ON academic.attendance_audit
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.attendance_policy (
    id                     uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid         NOT NULL,
    institution_id         uuid         NOT NULL,
    name                   text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    threshold_percent      numeric(5,2) NOT NULL CHECK (threshold_percent > 0 AND threshold_percent <= 100),
    -- Evaluate a student only after this many finalised sessions.
    min_sessions           integer      NOT NULL DEFAULT 1 CHECK (min_sessions >= 1),
    late_counts_as_present boolean      NOT NULL DEFAULT true,
    -- Excused sessions are left out of the denominator when true.
    exclude_excused        boolean      NOT NULL DEFAULT true,
    effective_from         date,
    status                 text         NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'in_review', 'effective', 'superseded', 'retired')),
    status_reason          text,
    process_instance_id    text,
    supersedes_id          uuid         REFERENCES academic.attendance_policy(id),
    version                bigint       NOT NULL DEFAULT 1,
    created_at             timestamptz  NOT NULL DEFAULT now(),
    updated_at             timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX attendance_policy_one_effective ON academic.attendance_policy (tenant_id, institution_id)
    WHERE status = 'effective';

CREATE TABLE academic.attendance_evaluation (
    id                uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid         NOT NULL,
    institution_id    uuid         NOT NULL,
    policy_id         uuid         NOT NULL REFERENCES academic.attendance_policy(id),
    section_id        uuid         NOT NULL REFERENCES academic.offering_section(id),
    student_id        uuid         NOT NULL,
    attended          integer      NOT NULL,
    total             integer      NOT NULL,
    percent           numeric(5,2) NOT NULL,
    below_threshold   boolean      NOT NULL,
    breached_at       timestamptz,
    evaluated_at      timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (section_id, student_id)
);
