-- Batch 7 slice 7A — HR cases (L2-E04 generic case lifecycle): probation reviews (L3-E-043),
-- separation cases (L3-E-041, archive L3-E-068), certificate requests (L3-E-039).

CREATE TABLE people.probation_review (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    review_date               date         NOT NULL,
    assessment                text,
    outcome                   text         NOT NULL CHECK (outcome IN ('confirmed', 'extended', 'terminated')),
    extended_until            date,
    previous_probation_end    date,
    status                    text         NOT NULL DEFAULT 'initiated'
        CHECK (status IN ('initiated', 'pending_approval', 'approved', 'rejected', 'processing', 'completed', 'failed', 'closed', 'archived')),
    status_reason             text,
    decided_by                text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now(),
    CHECK ((outcome = 'extended') = (extended_until IS NOT NULL))
);
CREATE UNIQUE INDEX probation_review_one_open
    ON people.probation_review (engagement_id) WHERE status IN ('initiated', 'pending_approval', 'approved', 'processing');
CREATE INDEX probation_review_engagement_idx ON people.probation_review (tenant_id, engagement_id, created_at);
CREATE TRIGGER probation_review_no_delete BEFORE DELETE ON people.probation_review
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

CREATE TABLE people.separation_case (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    case_number               text         NOT NULL CHECK (case_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    kind                      text         NOT NULL CHECK (kind IN ('resignation', 'retirement', 'termination', 'contract_end', 'death', 'other')),
    notice_date               date,
    last_working_date         date         NOT NULL,
    reason                    text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    -- Clearance checklist [{department, cleared, remarks}].
    clearance                 jsonb        NOT NULL DEFAULT '[]'::jsonb,
    clearance_complete        boolean      NOT NULL DEFAULT false,
    archived_profile          boolean      NOT NULL DEFAULT false,
    status                    text         NOT NULL DEFAULT 'initiated'
        CHECK (status IN ('initiated', 'pending_approval', 'approved', 'rejected', 'processing', 'completed', 'failed', 'closed', 'archived')),
    status_reason             text,
    decided_by                text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now(),
    CHECK (notice_date IS NULL OR notice_date <= last_working_date)
);
CREATE UNIQUE INDEX separation_case_number_per_institution ON people.separation_case (tenant_id, institution_id, case_number);
CREATE UNIQUE INDEX separation_case_one_open
    ON people.separation_case (engagement_id) WHERE status IN ('initiated', 'pending_approval', 'approved', 'processing');
CREATE INDEX separation_case_institution_idx ON people.separation_case (tenant_id, institution_id, status);
CREATE TRIGGER separation_case_no_delete BEFORE DELETE ON people.separation_case
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

CREATE TABLE people.certificate_request (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id             uuid         REFERENCES people.staff_engagement (id),
    kind                      text         NOT NULL CHECK (kind IN ('service', 'experience')),
    purpose                   text         NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 500),
    -- `self` (My HR) or `hr` (on behalf of the staff member).
    requested_via             text         NOT NULL CHECK (requested_via IN ('self', 'hr')),
    certificate_number        text         CHECK (certificate_number IS NULL OR certificate_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    -- What the issued certificate states (name, designation, service period).
    content                   jsonb,
    issued_at                 timestamptz,
    status                    text         NOT NULL DEFAULT 'initiated'
        CHECK (status IN ('initiated', 'pending_approval', 'approved', 'rejected', 'processing', 'completed', 'failed', 'closed', 'archived')),
    status_reason             text,
    decided_by                text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now(),
    CHECK ((status = 'completed') = (certificate_number IS NOT NULL))
);
CREATE UNIQUE INDEX certificate_request_number_per_institution
    ON people.certificate_request (tenant_id, institution_id, certificate_number) WHERE certificate_number IS NOT NULL;
CREATE INDEX certificate_request_staff_idx ON people.certificate_request (tenant_id, staff_id, created_at);
CREATE TRIGGER certificate_request_no_delete BEFORE DELETE ON people.certificate_request
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
