-- 4B — Academic monitoring (L2-D05 + purity amendment, L3-D-027…033).
--
-- A monitoring case over a governed scope (section, offering or one student of a section):
-- open → under_assessment → under_review → intervention → certified → closed. Risk assessments,
-- governance exceptions, governance decisions and interventions hang off the case. A reopening
-- never reverts a closed case: it creates a new case (`reopened_from_id`, `cycle_no + 1`) whose
-- first status is `reopened`. `source_key` columns make a repeated authoritative fact idempotent
-- (L4-D-027-BH-009, -028-BH-007, -030-BH-003, -031-BH-007, -032-BH-008, -033-BH-008).

CREATE TABLE academic.monitoring_case (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL,
    scope_kind           text        NOT NULL CHECK (scope_kind IN ('section', 'offering', 'student')),
    offering_id          uuid        REFERENCES academic.course_offering(id),
    section_id           uuid        REFERENCES academic.offering_section(id),
    student_id           uuid,
    title                text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- Monitoring plan / rule binding, e.g. {"attendanceThresholdPercent": 75}.
    plan                 jsonb       NOT NULL DEFAULT '{}'::jsonb,
    owner_subject        text        NOT NULL,
    reviewer_subject     text,
    status               text        NOT NULL DEFAULT 'open' CHECK (status IN (
        'open', 'under_assessment', 'intervention', 'under_review', 'certified', 'closed', 'reopened')),
    status_reason        text,
    status_changed_at    timestamptz NOT NULL DEFAULT now(),
    -- governance | closure | reopening while a decision is awaited.
    pending_decision     text        CHECK (pending_decision IN ('governance', 'closure', 'reopening')),
    pending_payload      jsonb,
    process_instance_id  text,
    outcome_summary      text,
    outcome_snapshot     jsonb,
    outcome_published_at timestamptz,
    certified_at         timestamptz,
    closed_at            timestamptz,
    closure_snapshot     jsonb,
    cycle_no             integer     NOT NULL DEFAULT 1,
    reopened_from_id     uuid        REFERENCES academic.monitoring_case(id),
    reopened_as_id       uuid        REFERENCES academic.monitoring_case(id),
    source_key           text,
    version              bigint      NOT NULL DEFAULT 1,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CHECK (scope_kind <> 'section' OR section_id IS NOT NULL),
    CHECK (scope_kind <> 'offering' OR offering_id IS NOT NULL),
    CHECK (scope_kind <> 'student' OR (student_id IS NOT NULL AND section_id IS NOT NULL))
);
CREATE UNIQUE INDEX monitoring_case_source ON academic.monitoring_case (tenant_id, source_key)
    WHERE source_key IS NOT NULL;
CREATE UNIQUE INDEX monitoring_case_one_reopening ON academic.monitoring_case (reopened_from_id)
    WHERE reopened_from_id IS NOT NULL;
CREATE INDEX monitoring_case_scope ON academic.monitoring_case (tenant_id, institution_id, status);
CREATE TRIGGER monitoring_case_no_delete BEFORE DELETE ON academic.monitoring_case
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.risk_assessment (
    id            uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id     uuid        NOT NULL,
    case_id       uuid        NOT NULL REFERENCES academic.monitoring_case(id),
    category      text        NOT NULL CHECK (length(btrim(category)) BETWEEN 1 AND 100),
    level         text        NOT NULL CHECK (level IN ('low', 'moderate', 'high', 'critical')),
    -- low..critical → 4..1 (1 = most urgent).
    priority      smallint    NOT NULL CHECK (priority BETWEEN 1 AND 4),
    -- How the level was set: `attendance_rule` (derived from evidence) or `reviewer`.
    basis         text        NOT NULL CHECK (basis IN ('attendance_rule', 'reviewer')),
    indicators    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    rationale     text,
    source_key    text,
    assessed_by   text,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX risk_assessment_source ON academic.risk_assessment (case_id, source_key)
    WHERE source_key IS NOT NULL;
CREATE TRIGGER risk_assessment_immutable BEFORE UPDATE OR DELETE ON academic.risk_assessment
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.monitoring_exception (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    case_id           uuid        NOT NULL REFERENCES academic.monitoring_case(id),
    category          text        NOT NULL CHECK (length(btrim(category)) BETWEEN 1 AND 100),
    severity          text        NOT NULL CHECK (severity IN ('low', 'moderate', 'high', 'critical')),
    description       text        NOT NULL CHECK (length(btrim(description)) > 0),
    evidence          text,
    owner_subject     text        NOT NULL,
    reviewer_subject  text        NOT NULL,
    resolution_plan   text,
    resolution_notes  text,
    status            text        NOT NULL DEFAULT 'raised' CHECK (status IN (
        'raised', 'planned', 'resolved', 'closed', 'escalated', 'withdrawn')),
    status_reason     text,
    validated_by      text,
    version           bigint      NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CHECK (owner_subject <> reviewer_subject)
);
CREATE INDEX monitoring_exception_case ON academic.monitoring_exception (case_id);

CREATE TABLE academic.monitoring_decision (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    case_id               uuid        NOT NULL REFERENCES academic.monitoring_case(id),
    kind                  text        NOT NULL CHECK (kind IN ('governance', 'closure', 'reopening')),
    decision              text        NOT NULL CHECK (decision IN ('approve', 'reject')),
    requires_intervention boolean     NOT NULL DEFAULT false,
    reason                text,
    payload               jsonb,
    process_instance_id   text,
    decided_by            text,
    created_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX monitoring_decision_case ON academic.monitoring_decision (case_id, created_at);
CREATE TRIGGER monitoring_decision_immutable BEFORE UPDATE OR DELETE ON academic.monitoring_decision
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.monitoring_intervention (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    case_id        uuid        NOT NULL REFERENCES academic.monitoring_case(id),
    -- The approved governance decision that requires it: one intervention per decision.
    decision_id    uuid        NOT NULL UNIQUE REFERENCES academic.monitoring_decision(id),
    title          text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    plan           text,
    owner_subject  text,
    due_on         date,
    status         text        NOT NULL DEFAULT 'planned' CHECK (status IN (
        'planned', 'triggered', 'in_progress', 'completed', 'cancelled')),
    status_reason  text,
    outcome        text,
    started_at     timestamptz,
    completed_at   timestamptz,
    version        bigint      NOT NULL DEFAULT 1,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX monitoring_intervention_case ON academic.monitoring_intervention (case_id);
