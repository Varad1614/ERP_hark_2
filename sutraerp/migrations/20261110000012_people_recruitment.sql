-- Batch 7 slice 7A — recruitment → onboarding (L2-E04; L3-E-033): requisitions, candidates,
-- onboarding cases. Joining creates the staff record + a pending engagement through the shared
-- inserters (`shared::staff`).

CREATE TABLE people.requisition (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    requisition_number        text         NOT NULL CHECK (requisition_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    title                     text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 160),
    position_id               uuid         REFERENCES people.position (id),
    org_unit_id               uuid,
    campus_id                 uuid,
    staff_category            text         NOT NULL CHECK (staff_category IN ('teaching', 'non_teaching', 'administrative')),
    employment_type           text         NOT NULL CHECK (employment_type IN ('permanent', 'probation', 'contract', 'visiting', 'temporary')),
    designation               text         NOT NULL CHECK (length(btrim(designation)) BETWEEN 1 AND 120),
    grade                     text         CHECK (grade IS NULL OR grade ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$'),
    openings                  integer      NOT NULL CHECK (openings BETWEEN 1 AND 500),
    justification             text,
    status                    text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'open', 'filled', 'cancelled')),
    status_reason             text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX requisition_number_per_institution ON people.requisition (tenant_id, institution_id, requisition_number);
CREATE INDEX requisition_institution_idx ON people.requisition (tenant_id, institution_id, status);
CREATE TRIGGER requisition_no_delete BEFORE DELETE ON people.requisition
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

CREATE TABLE people.candidate (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    requisition_id            uuid         NOT NULL REFERENCES people.requisition (id),
    given_name                text         NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 100),
    family_name               text         CHECK (family_name IS NULL OR length(btrim(family_name)) BETWEEN 1 AND 100),
    email                     text         NOT NULL CHECK (email ~ '^[^@\s]+@[^@\s]+$'),
    phone                     text         CHECK (phone IS NULL OR length(btrim(phone)) BETWEEN 3 AND 32),
    source                    text,
    notes                     text,
    offered_designation       text,
    offered_grade             text         CHECK (offered_grade IS NULL OR offered_grade ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$'),
    proposed_joining_date     date,
    status                    text         NOT NULL DEFAULT 'applied'
        CHECK (status IN ('applied', 'shortlisted', 'interviewed', 'offered', 'offer_accepted', 'offer_declined', 'rejected', 'withdrawn')),
    status_reason             text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX candidate_email_per_requisition ON people.candidate (requisition_id, lower(email));
CREATE INDEX candidate_requisition_idx ON people.candidate (tenant_id, requisition_id, status);
CREATE TRIGGER candidate_no_delete BEFORE DELETE ON people.candidate
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

CREATE TABLE people.onboarding_case (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    case_number               text         NOT NULL CHECK (case_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    candidate_id              uuid         NOT NULL REFERENCES people.candidate (id),
    requisition_id            uuid         NOT NULL REFERENCES people.requisition (id),
    joining_date              date         NOT NULL,
    subject_ref               text         CHECK (subject_ref IS NULL OR length(btrim(subject_ref)) BETWEEN 1 AND 200),
    org_unit_id               uuid,
    campus_id                 uuid,
    reports_to_staff_id       uuid         REFERENCES people.staff_member (id),
    probation_end_date        date,
    -- Submitted documents [{documentType, reference}] and the verification outcome.
    documents                 jsonb        NOT NULL DEFAULT '[]'::jsonb,
    documents_verified        boolean      NOT NULL DEFAULT false,
    staff_id                  uuid         REFERENCES people.staff_member (id),
    engagement_id             uuid         REFERENCES people.staff_engagement (id),
    status                    text         NOT NULL DEFAULT 'initiated'
        CHECK (status IN ('initiated', 'documentation_pending', 'verification_pending', 'ready_for_joining', 'joined', 'cancelled')),
    status_reason             text,
    process_instance_id       text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now(),
    CHECK ((status = 'joined') = (staff_id IS NOT NULL))
);
CREATE UNIQUE INDEX onboarding_case_number_per_institution ON people.onboarding_case (tenant_id, institution_id, case_number);
CREATE UNIQUE INDEX onboarding_case_one_per_candidate ON people.onboarding_case (candidate_id) WHERE status <> 'cancelled';
CREATE INDEX onboarding_case_institution_idx ON people.onboarding_case (tenant_id, institution_id, status);
CREATE TRIGGER onboarding_case_no_delete BEFORE DELETE ON people.onboarding_case
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
