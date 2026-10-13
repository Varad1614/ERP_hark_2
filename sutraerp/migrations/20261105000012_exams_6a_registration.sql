-- Batch 6 slice 6A: registration side tables + hall tickets (L2-I04, I05;
-- L3-I-017 … I-042). Registrations, registered papers and their mark rows themselves are the
-- foundation tables (written only through `shared::registration`).
--
--   registration_override     deadline override per candidate (DEADLINE_OVERRIDE approval, L3-I-020)
--   registration_arrangement  approved special examination arrangements (L3-I-029 / I-040)
--   registration_fee          late-registration fee raised through `ExamFeePort` (L3-I-019)
--   hall_ticket               entry authorisation per registration (`HT-` numbers, L3-I-037 … I-042)

CREATE TABLE exams.registration_override (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    student_id               uuid         NOT NULL,
    reason                   text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    valid_until              date         NOT NULL,
    status                   text         NOT NULL DEFAULT 'pending' CHECK (status IN (
                                 'pending', 'approved', 'rejected', 'used', 'expired')),
    status_reason            text,
    process_instance_id      text,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX registration_override_one_open ON exams.registration_override (session_id, student_id)
    WHERE status IN ('pending', 'approved');

CREATE TABLE exams.registration_arrangement (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    paper_id                 uuid         REFERENCES exams.exam_paper(id),
    kind                     text         NOT NULL CHECK (kind IN (
                                 'extra_time', 'scribe', 'separate_room', 'assistive_technology',
                                 'medical', 'other')),
    extra_minutes            integer      CHECK (extra_minutes IS NULL OR extra_minutes BETWEEN 1 AND 600),
    note                     text         NOT NULL CHECK (length(btrim(note)) BETWEEN 1 AND 1000),
    status                   text         NOT NULL DEFAULT 'approved' CHECK (status IN ('approved', 'revoked')),
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX registration_arrangement_registration_idx ON exams.registration_arrangement (registration_id);

CREATE TABLE exams.registration_fee (
    id                       uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid          NOT NULL,
    institution_id           uuid          NOT NULL,
    registration_id          uuid          NOT NULL UNIQUE REFERENCES exams.exam_registration(id),
    fee_code                 text          NOT NULL,
    amount                   numeric(12,2) NOT NULL CHECK (amount > 0),
    charge_id                uuid          NOT NULL,
    charge_number            text          NOT NULL,
    created_at               timestamptz   NOT NULL DEFAULT now()
);

CREATE TABLE exams.hall_ticket (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    registration_id          uuid         NOT NULL REFERENCES exams.exam_registration(id),
    student_id               uuid         NOT NULL,
    number                   text         NOT NULL,
    status                   text         NOT NULL DEFAULT 'active' CHECK (status IN (
                                 'draft', 'under_review', 'approved', 'published', 'active',
                                 'withheld', 'reissued', 'replaced', 'withdrawn', 'cancelled',
                                 'completed', 'archived')),
    -- Evidence of the entry-condition evaluation (dues, attendance per paper).
    conditions               jsonb        NOT NULL DEFAULT '{}'::jsonb,
    withheld_reason          text,
    special_arrangements     text,
    replaces_id              uuid         REFERENCES exams.hall_ticket(id),
    status_reason            text,
    process_instance_id      text,
    issued_at                timestamptz,
    version                  bigint       NOT NULL DEFAULT 1,
    created_by               text,
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, number)
);
-- L3-I-041: one current hall ticket per registration.
CREATE UNIQUE INDEX hall_ticket_one_current ON exams.hall_ticket (registration_id)
    WHERE status IN ('draft', 'under_review', 'approved', 'published', 'active', 'withheld');
CREATE INDEX hall_ticket_session_idx ON exams.hall_ticket (tenant_id, session_id);
CREATE TRIGGER hall_ticket_no_delete BEFORE DELETE ON exams.hall_ticket
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();
