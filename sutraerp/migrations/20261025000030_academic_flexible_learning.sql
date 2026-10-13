-- 4C — Flexible and external learning (L2-D06, L3-D-034…038, 040…042, 044, 045, 047, 048, 050).
--
-- Learning providers (onboarding → active), learning opportunities (open electives on a 4A
-- offering, MOOCs / SWAYAM / NPTEL / internships / external courses through a provider),
-- versioned flexible-learning policies, flexible enrolments (demand → eligibility → seat
-- allocation or waitlist → external registration → completion), seat allocations (one confirmed
-- allocation per enrolment, never above governed capacity), provider registrations, recorded
-- completions, exception cases, and an append-only status history of every 4C machine.
-- `source_key` / `evidence_key` columns make repeated authoritative facts idempotent
-- (L4-D-034-BH-008, -037-BH-008, -038-BH-008, -050-BH-007).

CREATE TABLE academic.learning_provider (
    id                       uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid        NOT NULL,
    institution_id           uuid        NOT NULL,
    code                     text        NOT NULL CHECK (code ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$'),
    name                     text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    provider_kind            text        NOT NULL CHECK (provider_kind IN (
        'mooc_platform', 'government_platform', 'university', 'industry', 'other')),
    -- External accreditation / approval evidence (referenced, never re-owned).
    accreditation_ref        text,
    accreditation_valid_until date,
    website                  text,
    contact_email            text,
    -- Approved institutional scope (modes the provider may deliver).
    modes                    text[]      NOT NULL DEFAULT '{}',
    scope_note               text,
    status                   text        NOT NULL DEFAULT 'proposed' CHECK (status IN (
        'proposed', 'under_review', 'active', 'suspended', 'offboarded')),
    status_reason            text,
    status_changed_at        timestamptz NOT NULL DEFAULT now(),
    process_instance_id      text,
    version                  bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at               timestamptz NOT NULL DEFAULT now(),
    updated_at               timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX learning_provider_code ON academic.learning_provider (tenant_id, institution_id, lower(code))
    WHERE status <> 'offboarded';
CREATE TRIGGER learning_provider_no_delete BEFORE DELETE ON academic.learning_provider
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.learning_opportunity (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL,
    code                 text        NOT NULL CHECK (code ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]{0,39}$'),
    title                text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    mode                 text        NOT NULL CHECK (mode IN (
        'open_elective', 'mooc', 'swayam', 'nptel', 'internship', 'external_course')),
    -- External opportunities: the approved provider. Open electives: the 4A offering.
    provider_id          uuid        REFERENCES academic.learning_provider (id),
    offering_id          uuid        REFERENCES academic.course_offering (id),
    academic_term_id     uuid        NOT NULL,
    credits              numeric(7,2) NOT NULL CHECK (credits >= 0 AND credits <= 100),
    min_hours            numeric(7,2) CHECK (min_hours IS NULL OR min_hours >= 0),
    capacity             integer     NOT NULL CHECK (capacity >= 0),
    reserved_seats       integer     NOT NULL DEFAULT 0 CHECK (reserved_seats >= 0),
    waitlist_enabled     boolean     NOT NULL DEFAULT true,
    -- Human review of enrolment requests (else the policy path approves validated requests).
    requires_approval    boolean     NOT NULL DEFAULT false,
    audience             text,
    enrolment_opens_on   date,
    enrolment_closes_on  date,
    status               text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'pending_approval', 'active', 'suspended', 'archived')),
    status_reason        text,
    status_changed_at    timestamptz NOT NULL DEFAULT now(),
    suspended_at         timestamptz,
    archived_at          timestamptz,
    -- Set by the D06 session archival (L3-D-047).
    session_archived_at  timestamptz,
    process_instance_id  text,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CHECK (reserved_seats <= capacity),
    CHECK (mode <> 'open_elective' OR offering_id IS NOT NULL),
    CHECK (mode = 'open_elective' OR provider_id IS NOT NULL),
    CHECK (enrolment_opens_on IS NULL OR enrolment_closes_on IS NULL OR enrolment_opens_on <= enrolment_closes_on)
);
CREATE UNIQUE INDEX learning_opportunity_code ON academic.learning_opportunity (tenant_id, institution_id, academic_term_id, lower(code))
    WHERE status <> 'archived';
CREATE INDEX learning_opportunity_scope ON academic.learning_opportunity (tenant_id, institution_id, status);
CREATE TRIGGER learning_opportunity_no_delete BEFORE DELETE ON academic.learning_opportunity
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Versioned eligibility / enrolment / withdrawal / exception rules + approval path (L3-D-045).
CREATE TABLE academic.flexible_learning_policy (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    name                text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    version_no          integer     NOT NULL CHECK (version_no >= 1),
    -- {maxConcurrentEnrolments, maxCreditsPerTerm, requireHumanApproval, waitlistValidityDays,
    --  withdrawalAllowedUntilStatus}
    rules               jsonb       NOT NULL,
    effective_from      date        NOT NULL,
    effective_to        date,
    status              text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'in_review', 'effective', 'superseded', 'retired')),
    status_reason       text,
    status_changed_at   timestamptz NOT NULL DEFAULT now(),
    supersedes_id       uuid        REFERENCES academic.flexible_learning_policy (id),
    process_instance_id text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_from <= effective_to)
);
CREATE UNIQUE INDEX flexible_learning_policy_version ON academic.flexible_learning_policy (tenant_id, institution_id, version_no);
CREATE UNIQUE INDEX flexible_learning_policy_one_effective ON academic.flexible_learning_policy (tenant_id, institution_id)
    WHERE status = 'effective';
CREATE TRIGGER flexible_learning_policy_no_delete BEFORE DELETE ON academic.flexible_learning_policy
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- A student's demand for an opportunity and its governed progression (L3-D-034…038).
CREATE TABLE academic.flexible_enrolment (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL,
    opportunity_id       uuid        NOT NULL REFERENCES academic.learning_opportunity (id),
    student_id           uuid        NOT NULL,
    student_context_id   uuid        NOT NULL,
    student_number       text        NOT NULL,
    display_name         text        NOT NULL,
    subject_ref          text,
    -- Policy version in force when the request was made (historical position, L4-D-045-BH-008).
    policy_id            uuid        REFERENCES academic.flexible_learning_policy (id),
    -- Lower number = higher priority (authoritative D06 priority policy).
    priority             integer     NOT NULL DEFAULT 100,
    status               text        NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'eligible', 'allocated', 'waitlisted', 'registered', 'in_progress',
        'completed', 'withdrawn', 'rejected')),
    status_reason        text,
    status_changed_at    timestamptz NOT NULL DEFAULT now(),
    eligibility          jsonb       NOT NULL DEFAULT '{}'::jsonb,
    waitlisted_at        timestamptz,
    waitlist_expires_on  date,
    allocated_at         timestamptz,
    hours_completed      numeric(7,2),
    source_key           text,
    process_instance_id  text,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
-- One live demand per student and opportunity (L4-D-034-BH-008).
CREATE UNIQUE INDEX flexible_enrolment_one_live ON academic.flexible_enrolment (opportunity_id, student_id)
    WHERE status NOT IN ('withdrawn', 'rejected');
CREATE UNIQUE INDEX flexible_enrolment_source ON academic.flexible_enrolment (tenant_id, source_key)
    WHERE source_key IS NOT NULL;
CREATE INDEX flexible_enrolment_opportunity ON academic.flexible_enrolment (tenant_id, opportunity_id, status);
CREATE INDEX flexible_enrolment_student ON academic.flexible_enrolment (tenant_id, student_id);
CREATE TRIGGER flexible_enrolment_no_delete BEFORE DELETE ON academic.flexible_enrolment
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Governed seat allocations: at most one confirmed allocation per enrolment (L4-D-035-BH-004).
CREATE TABLE academic.flexible_seat_allocation (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    opportunity_id  uuid        NOT NULL REFERENCES academic.learning_opportunity (id),
    enrolment_id    uuid        NOT NULL REFERENCES academic.flexible_enrolment (id),
    -- allocation | waitlist_promotion | conflict_resolution
    source          text        NOT NULL CHECK (source IN ('allocation', 'waitlist_promotion', 'conflict_resolution')),
    status          text        NOT NULL DEFAULT 'confirmed' CHECK (status IN ('confirmed', 'released')),
    release_reason  text,
    released_at     timestamptz,
    allocated_by    text,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX flexible_seat_allocation_one ON academic.flexible_seat_allocation (enrolment_id)
    WHERE status = 'confirmed';
CREATE INDEX flexible_seat_allocation_opportunity ON academic.flexible_seat_allocation (opportunity_id, status);
CREATE TRIGGER flexible_seat_allocation_no_delete BEFORE DELETE ON academic.flexible_seat_allocation
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Provider-side registration of an approved participation (L3-D-037).
CREATE TABLE academic.external_registration (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    enrolment_id        uuid        NOT NULL REFERENCES academic.flexible_enrolment (id),
    provider_id         uuid        NOT NULL REFERENCES academic.learning_provider (id),
    status              text        NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'accepted', 'rejected', 'recovery_required')),
    provider_reference  text,
    -- Provider acknowledgement id: a repeated acknowledgement is the same fact.
    evidence_key        text,
    outcome_reason      text,
    requested_by        text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX external_registration_live ON academic.external_registration (enrolment_id)
    WHERE status IN ('requested', 'accepted');
CREATE UNIQUE INDEX external_registration_evidence ON academic.external_registration (tenant_id, provider_id, evidence_key)
    WHERE evidence_key IS NOT NULL;
CREATE TRIGGER external_registration_no_delete BEFORE DELETE ON academic.external_registration
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Recorded institutional learning completion (L3-D-038): one per enrolment.
CREATE TABLE academic.learning_completion (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    enrolment_id          uuid        NOT NULL REFERENCES academic.flexible_enrolment (id),
    hours_completed       numeric(7,2) NOT NULL CHECK (hours_completed >= 0),
    assessment_status     text        NOT NULL CHECK (assessment_status IN ('passed', 'failed', 'not_applicable')),
    grade                 text,
    provider_evidence_ref text,
    completed_on          date        NOT NULL,
    source_key            text,
    recorded_by           text,
    created_at            timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX learning_completion_one ON academic.learning_completion (enrolment_id);
CREATE TRIGGER learning_completion_append_only BEFORE UPDATE OR DELETE ON academic.learning_completion
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Flexible-learning exception case → governed closure (L3-D-050).
CREATE TABLE academic.flexible_exception_case (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    opportunity_id      uuid        REFERENCES academic.learning_opportunity (id),
    enrolment_id        uuid        REFERENCES academic.flexible_enrolment (id),
    category            text        NOT NULL CHECK (category IN (
        'allocation', 'registration', 'completion', 'certificate', 'recognition', 'reconciliation', 'other')),
    title               text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description         text,
    root_cause          text,
    owner_subject       text        NOT NULL,
    corrective_plan     text,
    override_required   boolean     NOT NULL DEFAULT false,
    override_justification text,
    corrective_action   text,
    resolution_evidence text,
    on_hold             boolean     NOT NULL DEFAULT false,
    hold_reason         text,
    status              text        NOT NULL DEFAULT 'open' CHECK (status IN (
        'open', 'plan_pending', 'plan_approved', 'resolved', 'closed')),
    status_reason       text,
    status_changed_at   timestamptz NOT NULL DEFAULT now(),
    source_key          text,
    process_instance_id text,
    closed_at           timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX flexible_exception_case_source ON academic.flexible_exception_case (tenant_id, source_key)
    WHERE source_key IS NOT NULL;
CREATE INDEX flexible_exception_case_scope ON academic.flexible_exception_case (tenant_id, institution_id, status);
CREATE TRIGGER flexible_exception_case_no_delete BEFORE DELETE ON academic.flexible_exception_case
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Append-only status history of every 4C machine (suspension history, queue history, …).
CREATE TABLE academic.flexible_status_history (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    aggregate_type      text        NOT NULL,
    aggregate_id        uuid        NOT NULL,
    from_status         text,
    to_status           text        NOT NULL,
    reason              text,
    process_instance_id text,
    actor               text,
    correlation_id      text,
    created_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX flexible_status_history_aggregate ON academic.flexible_status_history (aggregate_id, created_at);
CREATE TRIGGER flexible_status_history_append_only BEFORE UPDATE OR DELETE ON academic.flexible_status_history
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
