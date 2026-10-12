-- Batch 3 slice 3C: scholarship definitions, eligibility policies, student eligibility,
-- assessments, external sync and bulk recalculation (L2-C07, L3-C-062 … C-071).
-- Financial aid is Finance-owned (Batch 5): the hand-off is the `scholarship_eligibility.handed_off`
-- event only; nothing here references Finance tables.

CREATE TABLE student.scholarship (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    code                   text        NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                   text        NOT NULL CHECK (length(btrim(name)) > 0),
    description            text,
    sponsor_type           text        NOT NULL CHECK (sponsor_type IN ('institutional',
                                           'government', 'private', 'external')),
    -- External ecosystem scheme code (e.g. an NSP scheme id) for L3-C-069.
    external_scheme        text,
    benefit_type           text        NOT NULL CHECK (benefit_type IN ('fixed_amount',
                                           'fee_percentage', 'fee_waiver', 'stipend')),
    benefit_value          numeric(12,2),
    currency               text        NOT NULL DEFAULT 'INR',
    valid_from             date        NOT NULL,
    valid_until            date,
    renewable              boolean     NOT NULL DEFAULT false,
    status                 text        NOT NULL CHECK (status IN ('draft', 'published', 'active',
                                           'suspended', 'expired', 'archived')),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until IS NULL OR valid_until >= valid_from)
);
CREATE UNIQUE INDEX scholarship_code_per_institution
    ON student.scholarship (tenant_id, institution_id, code);

CREATE TABLE student.scholarship_policy (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    scholarship_id         uuid        NOT NULL REFERENCES student.scholarship (id),
    version_no             integer     NOT NULL,
    status                 text        NOT NULL CHECK (status IN ('draft', 'in_review', 'effective',
                                           'superseded', 'retired')),
    -- Criteria (camelCase JSON): minAcademicScore, maxFamilyIncome, categories[], programmeIds[],
    -- minTermNo, requiredDocuments[], conditionalDocuments (bool), validityDays.
    criteria               jsonb       NOT NULL DEFAULT '{}'::jsonb,
    effective_from         date        NOT NULL,
    process_instance_id    text,
    decision_reason        text,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (scholarship_id, version_no)
);
CREATE UNIQUE INDEX scholarship_policy_one_effective
    ON student.scholarship_policy (scholarship_id) WHERE status = 'effective';
CREATE UNIQUE INDEX scholarship_policy_one_open
    ON student.scholarship_policy (scholarship_id) WHERE status IN ('draft', 'in_review');

CREATE TABLE student.scholarship_eligibility (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    scholarship_id         uuid        NOT NULL REFERENCES student.scholarship (id),
    policy_id              uuid        REFERENCES student.scholarship_policy (id),
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    academic_year_id       uuid,
    status                 text        NOT NULL CHECK (status IN ('created', 'assessment_pending',
                                           'under_evaluation', 'eligible', 'not_eligible',
                                           'conditionally_eligible', 'recommendation_generated',
                                           'active', 'reassessment', 'renewal', 'expired',
                                           'archived')),
    -- Applicant facts supplied with the request ({academicScore, familyIncome, category, documents[]}).
    facts                  jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- Last evaluation ({outcome, criteria:[{criterion, met, detail}], conditions[]}).
    evaluation             jsonb,
    conditions             text[]      NOT NULL DEFAULT '{}',
    condition_deadline     date,
    recommendation_pending boolean     NOT NULL DEFAULT false,
    process_instance_id    text,
    valid_from             date,
    valid_until            date,
    renewal_count          integer     NOT NULL DEFAULT 0,
    handed_off_at          timestamptz,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- One current eligibility per student and scholarship (expired / archived ones are history).
CREATE UNIQUE INDEX scholarship_eligibility_one_current
    ON student.scholarship_eligibility (tenant_id, scholarship_id, student_id)
    WHERE status NOT IN ('expired', 'archived', 'not_eligible');
CREATE INDEX scholarship_eligibility_student_idx
    ON student.scholarship_eligibility (tenant_id, student_id);

-- Every evaluation is appended, never rewritten (L4-C-066 / C-070).
CREATE TABLE student.scholarship_eligibility_assessment (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    eligibility_id         uuid        NOT NULL REFERENCES student.scholarship_eligibility (id),
    kind                   text        NOT NULL CHECK (kind IN ('determination', 'condition_resolution',
                                           'reassessment', 'renewal', 'recalculation')),
    policy_id              uuid,
    from_status            text,
    to_status              text        NOT NULL,
    facts                  jsonb       NOT NULL DEFAULT '{}'::jsonb,
    evaluation             jsonb,
    reason                 text,
    recalculation_id       uuid,
    actor                  text,
    recorded_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX scholarship_eligibility_assessment_idx
    ON student.scholarship_eligibility_assessment (eligibility_id, recorded_at);
CREATE TRIGGER scholarship_eligibility_assessment_immutable
    BEFORE UPDATE OR DELETE ON student.scholarship_eligibility_assessment
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.scholarship_external_sync (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    scholarship_id         uuid        NOT NULL REFERENCES student.scholarship (id),
    external_scheme        text        NOT NULL,
    student_count          integer     NOT NULL,
    -- ExternalSyncOutcome kind: accepted | rejected | not_configured.
    outcome                text        NOT NULL CHECK (outcome IN ('accepted', 'rejected', 'not_configured')),
    reference              text,
    detail                 text,
    as_of                  date        NOT NULL,
    actor                  text,
    created_at             timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER scholarship_external_sync_immutable
    BEFORE UPDATE OR DELETE ON student.scholarship_external_sync
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.scholarship_recalculation (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    scholarship_id         uuid        REFERENCES student.scholarship (id),
    reason                 text,
    evaluated              integer     NOT NULL DEFAULT 0,
    changed                integer     NOT NULL DEFAULT 0,
    -- [{eligibilityId, studentId, fromStatus, toStatus}]
    changes                jsonb       NOT NULL DEFAULT '[]'::jsonb,
    status                 text        NOT NULL DEFAULT 'completed' CHECK (status IN ('completed')),
    actor                  text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER scholarship_recalculation_immutable
    BEFORE UPDATE OR DELETE ON student.scholarship_recalculation
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
