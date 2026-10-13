-- Batch 6 slice 6A: examination policy versions + governed session revisions
-- (docs/batch6-contracts.md §3.1 range 20261105000010 … 029; L2-I01, L2-I02; L3-I-001 … I-006).
--
--   exam_policy            policy versions per institution (draft → under_review → approved →
--                          scheduled / active → superseded / retired → archived); at most ONE
--                          active version per institution; carries the rules 6A enforces
--                          (registration window, late registration + fee, hall-ticket
--                          conditions, attempt limit, invigilation ratio, hall-ticket approval).
--   exam_session_revision  append-only record of every governed change of a session after draft
--                          (revision, duplicate merge, registration reopening, population and
--                          hall-ticket finalisation) with before / after values and a reason.

CREATE TABLE exams.exam_policy (
    id                           uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                    uuid          NOT NULL,
    institution_id               uuid          NOT NULL,
    code                         text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    version_no                   integer       NOT NULL DEFAULT 1 CHECK (version_no BETWEEN 1 AND 999),
    name                         text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    effective_from               date,
    registration_window_enforced boolean       NOT NULL DEFAULT true,
    late_registration_allowed    boolean       NOT NULL DEFAULT true,
    late_fee_code                text          CHECK (late_fee_code IS NULL OR late_fee_code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    late_fee_amount              numeric(12,2) CHECK (late_fee_amount IS NULL OR late_fee_amount >= 0),
    require_dues_cleared         boolean       NOT NULL DEFAULT true,
    min_attendance_percent       numeric(5,2)  CHECK (min_attendance_percent IS NULL
                                     OR (min_attendance_percent >= 0 AND min_attendance_percent <= 100)),
    max_attempts                 integer       NOT NULL DEFAULT 3 CHECK (max_attempts BETWEEN 1 AND 20),
    invigilator_ratio            integer       NOT NULL DEFAULT 30 CHECK (invigilator_ratio BETWEEN 1 AND 500),
    hall_ticket_approval         boolean       NOT NULL DEFAULT false,
    self_service_allowed         boolean       NOT NULL DEFAULT true,
    supersedes_id                uuid          REFERENCES exams.exam_policy(id),
    status                       text          NOT NULL DEFAULT 'draft' CHECK (status IN (
                                     'draft', 'under_review', 'approved', 'scheduled', 'active',
                                     'superseded', 'retired', 'archived')),
    status_reason                text,
    process_instance_id          text,
    version                      bigint        NOT NULL DEFAULT 1,
    created_by                   text,
    created_at                   timestamptz   NOT NULL DEFAULT now(),
    updated_at                   timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code, version_no)
);
-- L2-I01: one authoritative (active) policy per institution.
CREATE UNIQUE INDEX exam_policy_one_active ON exams.exam_policy (tenant_id, institution_id)
    WHERE status = 'active';
CREATE INDEX exam_policy_institution_idx ON exams.exam_policy (tenant_id, institution_id, code);
CREATE TRIGGER exam_policy_no_delete BEFORE DELETE ON exams.exam_policy
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

CREATE TABLE exams.exam_session_revision (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    session_id               uuid         NOT NULL REFERENCES exams.exam_session(id),
    kind                     text         NOT NULL CHECK (kind IN (
                                 'revision', 'merge', 'registration_reopened',
                                 'registration_finalised', 'hall_tickets_finalised')),
    changed_fields           text[]       NOT NULL DEFAULT '{}',
    before_values            jsonb        NOT NULL DEFAULT '{}'::jsonb,
    after_values             jsonb        NOT NULL DEFAULT '{}'::jsonb,
    reason                   text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    merged_into_session_id   uuid         REFERENCES exams.exam_session(id),
    session_status           text         NOT NULL,
    session_version          bigint       NOT NULL,
    actor                    text,
    created_at               timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX exam_session_revision_session_idx
    ON exams.exam_session_revision (tenant_id, session_id, created_at);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.exam_session_revision FROM sutra_exams;
CREATE TRIGGER exam_session_revision_append_only BEFORE UPDATE OR DELETE ON exams.exam_session_revision
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();
