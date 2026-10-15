-- Part 1 · Stage 3 (S3-appr) — DEF-P1-DEMO-006: a section-seat eligibility override is a governed
-- request with a decision endpoint. In human approval mode the override used to wait for a
-- decision that no endpoint could make (409 for ever). Now
--   POST /api/v1/academic/sections/{id}/enrolment-overrides      (offering.enrolment.manage, maker)
-- records the request here (status `pending`), and
--   POST /api/v1/academic/enrolment-overrides/{id}/decision       (academic.offering.decide, checker)
-- seats the student (approve) or closes it (reject). The checker is never the requester
-- (separation of duties, `requested_by` + the approval request ledger).

CREATE TABLE academic.enrolment_override_request (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL,
    offering_id          uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id           uuid        NOT NULL REFERENCES academic.offering_section (id),
    student_id           uuid        NOT NULL,
    home_institution_id  uuid,
    reason               text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    status               text        NOT NULL DEFAULT 'pending'
                                     CHECK (status IN ('pending', 'approved', 'rejected')),
    requested_by         text,
    decided_by           text,
    decided_at           timestamptz,
    decision_reason      text,
    enrolment_id         uuid        REFERENCES academic.section_enrolment (id),
    process_instance_id  text,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    -- a decided request names its decider; an approved one names the seat it created
    CHECK (status = 'pending' OR (decided_by IS NOT NULL AND decided_at IS NOT NULL)),
    CHECK (status <> 'approved' OR enrolment_id IS NOT NULL),
    -- separation of duties at the data level as well
    CHECK (decided_by IS NULL OR requested_by IS NULL OR decided_by <> requested_by)
);

CREATE INDEX enrolment_override_request_section_idx
    ON academic.enrolment_override_request (tenant_id, section_id, status);
-- at most one open request per student and section
CREATE UNIQUE INDEX enrolment_override_request_open_uq
    ON academic.enrolment_override_request (tenant_id, section_id, student_id)
    WHERE status = 'pending';
