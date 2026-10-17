-- ERP_EXTENSIONS 2A — admissions funnel. Logic ported from HeyPuter/everything crm
-- lib/funnel.js, forecast.js and dupes.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- An enquiry is a prospective applicant before (and while) they apply: where they came from,
-- the funnel stage they are in with its full stage history, an expected joining date, a fee
-- value and an optional confidence for the forecast, and the application it turned into. The
-- terminal stages are `admitted` and `lost` (a lost enquiry carries a reason). Each institution
-- may configure its own open stages and per-stage probabilities, a stale threshold, and seat
-- targets per programme and intake.
CREATE TABLE student.admission_enquiry (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    enquiry_number       text           NOT NULL,
    programme_id         uuid,
    intake               text           CHECK (intake IS NULL OR intake ~ '^[0-9]{4}(-[0-9]{2})?$'),
    given_name           text           NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 100),
    family_name          text,
    email                text,
    phone                text,
    guardian_name        text,
    guardian_phone       text,
    city                 text,
    source               text           NOT NULL DEFAULT 'walk_in' CHECK (source IN (
                                            'walk_in', 'website', 'phone', 'referral', 'campaign',
                                            'education_fair', 'social', 'agent', 'school_visit',
                                            'other')),
    campaign             text,
    stage                text           NOT NULL CHECK (stage ~ '^[a-z][a-z0-9_]{1,31}$'),
    -- [{ "stage": "...", "at": <ms since epoch> }, ...] in order; the last entry is `stage`.
    stage_history        jsonb          NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(stage_history) = 'array'),
    stage_entered_at     timestamptz    NOT NULL DEFAULT now(),
    expected_join        date,
    expected_fee         numeric(18,2)  NOT NULL DEFAULT 0 CHECK (expected_fee >= 0),
    confidence           numeric(4,3)   CHECK (confidence IS NULL OR confidence BETWEEN 0 AND 1),
    close_reason         text,
    closed_at            timestamptz,
    owner_ref            text,
    next_follow_up       date,
    notes                text,
    application_id       uuid           REFERENCES student.admission_application (id),
    version              integer        NOT NULL DEFAULT 1,
    created_by           text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, enquiry_number),
    CHECK (stage <> 'lost' OR close_reason IS NOT NULL),
    CHECK ((stage IN ('admitted', 'lost')) = (closed_at IS NOT NULL))
);
CREATE INDEX admission_enquiry_institution_idx
    ON student.admission_enquiry (tenant_id, institution_id, created_at DESC);
CREATE INDEX admission_enquiry_stage_idx
    ON student.admission_enquiry (tenant_id, institution_id, stage);
-- One enquiry per application.
CREATE UNIQUE INDEX admission_enquiry_application_once
    ON student.admission_enquiry (tenant_id, application_id) WHERE application_id IS NOT NULL;

CREATE TABLE student.admission_funnel_settings (
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    -- Open stages [{ id, label, probability }] in order (empty = the defaults).
    stages               jsonb          NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(stages) = 'array'),
    stale_days           integer        NOT NULL DEFAULT 14 CHECK (stale_days BETWEEN 0 AND 365),
    applied_stage        text,
    offered_stage        text,
    version              integer        NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

CREATE TABLE student.admission_intake_target (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    programme_id         uuid           NOT NULL,
    intake               text           NOT NULL CHECK (intake ~ '^[0-9]{4}(-[0-9]{2})?$'),
    seats                integer        NOT NULL CHECK (seats BETWEEN 0 AND 100000),
    updated_by           text,
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, programme_id, intake)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON student.admission_enquiry,
    student.admission_funnel_settings, student.admission_intake_target TO sutra_student;
-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
