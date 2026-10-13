-- Batch 6 slice 6A: schedules / timetables, sittings, venues + allocation, seating, invigilation,
-- conduct records + attendance, examination exceptions (L2-I03, I06, I07, I15;
-- L3-I-007 … I-016, I-043, I-055, I-057, I-059, I-060, I-172, I-174, I-175, I-185).

CREATE TABLE exams.exam_schedule (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    name                     text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    version_no               integer      NOT NULL DEFAULT 1 CHECK (version_no BETWEEN 1 AND 999),
    supersedes_id            uuid         REFERENCES exams.exam_schedule(id),
    status                   text         NOT NULL DEFAULT 'draft' CHECK (status IN (
                                 'draft', 'under_review', 'approved', 'published', 'active',
                                 'revised', 'cancelled', 'completed', 'archived')),
    status_reason            text,
    process_instance_id      text,
    published_at             timestamptz,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (session_id, version_no)
);
-- One live (not revised / cancelled / completed / archived) schedule per session.
CREATE UNIQUE INDEX exam_schedule_one_live ON exams.exam_schedule (session_id)
    WHERE status IN ('draft', 'under_review', 'approved', 'published', 'active');
CREATE TRIGGER exam_schedule_no_delete BEFORE DELETE ON exams.exam_schedule
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.exam_sitting (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    schedule_id              uuid         NOT NULL REFERENCES exams.exam_schedule(id),
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    paper_id                 uuid         NOT NULL REFERENCES exams.exam_paper(id),
    sits_on                  date         NOT NULL,
    starts_at                time         NOT NULL,
    duration_minutes         integer      NOT NULL CHECK (duration_minutes BETWEEN 1 AND 1440),
    status                   text         NOT NULL DEFAULT 'planned' CHECK (status IN (
                                 'planned', 'postponed', 'cancelled', 'completed')),
    status_reason            text,
    postponed_from           date,
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX exam_sitting_one_per_paper ON exams.exam_sitting (schedule_id, paper_id)
    WHERE status <> 'cancelled';
CREATE INDEX exam_sitting_session_idx ON exams.exam_sitting (tenant_id, session_id, sits_on);

CREATE TABLE exams.exam_venue (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    code                     text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                     text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    capacity                 integer      NOT NULL CHECK (capacity BETWEEN 1 AND 10000),
    location                 text,
    status                   text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE exams.venue_allocation (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    sitting_id               uuid         NOT NULL REFERENCES exams.exam_sitting(id),
    venue_id                 uuid         NOT NULL REFERENCES exams.exam_venue(id),
    seats                    integer      NOT NULL CHECK (seats BETWEEN 1 AND 10000),
    status                   text         NOT NULL DEFAULT 'allocated' CHECK (status IN ('allocated', 'released')),
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX venue_allocation_one_live ON exams.venue_allocation (sitting_id, venue_id)
    WHERE status = 'allocated';
CREATE INDEX venue_allocation_venue_idx ON exams.venue_allocation (venue_id);

CREATE TABLE exams.seating_plan (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    sitting_id               uuid         NOT NULL REFERENCES exams.exam_sitting(id),
    plan_no                  integer      NOT NULL DEFAULT 1,
    supersedes_id            uuid         REFERENCES exams.seating_plan(id),
    candidates               integer      NOT NULL DEFAULT 0,
    status                   text         NOT NULL DEFAULT 'draft' CHECK (status IN (
                                 'draft', 'published', 'revised', 'cancelled')),
    status_reason            text,
    published_at             timestamptz,
    version                  bigint       NOT NULL DEFAULT 1,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (sitting_id, plan_no)
);
CREATE UNIQUE INDEX seating_plan_one_live ON exams.seating_plan (sitting_id)
    WHERE status IN ('draft', 'published');

CREATE TABLE exams.seat_assignment (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    plan_id                  uuid         NOT NULL REFERENCES exams.seating_plan(id),
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    student_id               uuid         NOT NULL,
    student_number           text         NOT NULL,
    display_name             text         NOT NULL,
    venue_id                 uuid         NOT NULL REFERENCES exams.exam_venue(id),
    seat_no                  text         NOT NULL,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (plan_id, registration_id),
    UNIQUE (plan_id, venue_id, seat_no)
);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.seat_assignment FROM sutra_exams;
CREATE TRIGGER seat_assignment_append_only BEFORE UPDATE OR DELETE ON exams.seat_assignment
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.invigilation_duty (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    sitting_id               uuid         NOT NULL REFERENCES exams.exam_sitting(id),
    venue_id                 uuid         REFERENCES exams.exam_venue(id),
    subject_ref              text         NOT NULL CHECK (length(btrim(subject_ref)) BETWEEN 1 AND 200),
    name                     text,
    role                     text         NOT NULL DEFAULT 'invigilator' CHECK (role IN (
                                 'invigilator', 'chief_invigilator', 'practical_examiner',
                                 'external_examiner')),
    replaces_id              uuid         REFERENCES exams.invigilation_duty(id),
    status                   text         NOT NULL DEFAULT 'assigned' CHECK (status IN (
                                 'assigned', 'confirmed', 'replaced', 'released', 'completed')),
    status_reason            text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX invigilation_duty_one_live ON exams.invigilation_duty (sitting_id, subject_ref)
    WHERE status IN ('assigned', 'confirmed');
CREATE INDEX invigilation_duty_subject_idx ON exams.invigilation_duty (tenant_id, subject_ref);

CREATE TABLE exams.conduct_record (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    sitting_id               uuid         NOT NULL REFERENCES exams.exam_sitting(id),
    paper_id                 uuid         NOT NULL REFERENCES exams.exam_paper(id),
    status                   text         NOT NULL DEFAULT 'scheduled' CHECK (status IN (
                                 'scheduled', 'in_progress', 'suspended', 'completed',
                                 'terminated', 'postponed')),
    status_reason            text,
    started_at               timestamptz,
    completed_at             timestamptz,
    present_count            integer      NOT NULL DEFAULT 0,
    absent_count             integer      NOT NULL DEFAULT 0,
    denied_count             integer      NOT NULL DEFAULT 0,
    pending_postpone         jsonb,
    process_instance_id      text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX conduct_record_one_live ON exams.conduct_record (sitting_id)
    WHERE status NOT IN ('postponed', 'terminated');

CREATE TABLE exams.conduct_attendance (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    conduct_id               uuid         NOT NULL REFERENCES exams.conduct_record(id),
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    registration_paper_id    uuid         NOT NULL REFERENCES exams.exam_registration_paper(id),
    student_id               uuid         NOT NULL,
    outcome                  text         NOT NULL CHECK (outcome IN ('present', 'absent', 'denied_entry')),
    reason                   text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (conduct_id, registration_id)
);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.conduct_attendance FROM sutra_exams;
CREATE TRIGGER conduct_attendance_append_only BEFORE UPDATE OR DELETE ON exams.conduct_attendance
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.exam_exception (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    number                   text         NOT NULL,
    sitting_id               uuid         REFERENCES exams.exam_sitting(id),
    paper_id                 uuid         REFERENCES exams.exam_paper(id),
    registration_id          uuid         REFERENCES exams.exam_registration(id),
    category                 text         NOT NULL CHECK (category IN (
                                 'conduct', 'venue', 'paper', 'candidate', 'technical', 'other')),
    classification           text,
    severity                 text         CHECK (severity IS NULL OR severity IN ('low', 'medium', 'high', 'critical')),
    description              text         NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 2000),
    resolution               text,
    status                   text         NOT NULL DEFAULT 'raised' CHECK (status IN (
                                 'raised', 'classified', 'under_review', 'resolved', 'closed')),
    status_reason            text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, number)
);
CREATE INDEX exam_exception_session_idx ON exams.exam_exception (tenant_id, session_id);
