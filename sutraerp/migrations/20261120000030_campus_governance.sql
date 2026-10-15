-- Batch 9 slice 9E — campus governance (Domain G00; L3-G-001…015; docs/batch9-contracts.md §12.1).
--
-- One governed case table for every G00 aggregate kind (assessments, reviews, risks, action
-- plans, SLA deviations, KPI reviews, findings, escalations, exceptions), child items
-- (criteria, recommendations, actions, corrective actions), versioned governance configuration
-- and governance evidence published to accreditation (event
-- `sutraerp.campus.governance_evidence.published`, never read by SQL from another schema).
-- Approval state uses the canonical wire values of contract §6.4.

CREATE TABLE campus.governance_case (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    kind                 text         NOT NULL CHECK (kind IN ('readiness_assessment', 'health_assessment',
                                          'maturity_assessment', 'operational_review', 'executive_review', 'risk',
                                          'action_plan', 'sla_deviation', 'kpi_review', 'finding', 'escalation',
                                          'exception')),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description          text         CHECK (description IS NULL OR length(description) <= 4000),
    -- Campus domains concerned (facilities, hostel, transport, library, laboratory, service_desk …).
    domains              text[]       NOT NULL DEFAULT '{}',
    severity             text         NOT NULL DEFAULT 'medium' CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    owner                text,
    due_on               date,
    -- Risk register (G-005): inherent likelihood × impact, residual score at closure.
    likelihood           integer      CHECK (likelihood IS NULL OR likelihood BETWEEN 1 AND 5),
    impact               integer      CHECK (impact IS NULL OR impact BETWEEN 1 AND 5),
    residual_score       integer      CHECK (residual_score IS NULL OR residual_score BETWEEN 1 AND 25),
    -- Exceptions (G-013).
    exception_type       text         CHECK (exception_type IS NULL OR exception_type IN ('evidence', 'dependency')),
    expires_on           date,
    -- Escalations (G-012): the escalated case.
    parent_id            uuid         REFERENCES campus.governance_case (id),
    -- Computed figures (assessment score, SLA breach snapshot, KPI snapshot …).
    facts                jsonb        NOT NULL DEFAULT '{}'::jsonb,
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'open', 'pending_decision', 'closed', 'cancelled')),
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    -- Proposed (pending) / decided outcome, kind specific (`ready`, `mitigated`, `level_3` …).
    outcome              text,
    rationale            text         CHECK (rationale IS NULL OR length(rationale) <= 4000),
    status_reason        text,
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (kind <> 'escalation' OR parent_id IS NOT NULL),
    CHECK (kind <> 'exception' OR (exception_type IS NOT NULL AND expires_on IS NOT NULL)),
    CHECK (kind <> 'risk' OR (likelihood IS NOT NULL AND impact IS NOT NULL))
);
CREATE UNIQUE INDEX governance_case_number ON campus.governance_case (tenant_id, institution_id, number);
CREATE INDEX governance_case_inst ON campus.governance_case (tenant_id, institution_id, kind, status);
CREATE INDEX governance_case_parent ON campus.governance_case (tenant_id, parent_id);
CREATE TRIGGER governance_case_no_delete BEFORE DELETE ON campus.governance_case
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

CREATE TABLE campus.governance_item (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    case_id              uuid         NOT NULL REFERENCES campus.governance_case (id),
    kind                 text         NOT NULL CHECK (kind IN ('criterion', 'recommendation', 'action', 'corrective_action')),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    domain               text,
    owner                text,
    due_on               date,
    -- Criterion rating 1..5 (assessments).
    score                integer      CHECK (score IS NULL OR score BETWEEN 1 AND 5),
    status               text         NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'in_progress', 'done', 'verified', 'cancelled')),
    note                 text,
    evidence_ref         text,
    done_by              text,
    done_at              timestamptz,
    verified_by          text,
    verified_at          timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (status <> 'verified' OR verified_by IS NOT NULL)
);
CREATE INDEX governance_item_case ON campus.governance_item (tenant_id, case_id, created_at);
CREATE TRIGGER governance_item_no_delete BEFORE DELETE ON campus.governance_item
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- Versioned governance configuration (G-014): one effective version per key; history kept.
CREATE TABLE campus.governance_config_version (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    key                  text         NOT NULL CHECK (key IN ('readiness_threshold', 'risk_appetite',
                                          'review_cadence', 'kpi_thresholds')),
    version_no           integer      NOT NULL CHECK (version_no >= 1),
    settings             jsonb        NOT NULL,
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'effective', 'superseded', 'withdrawn')),
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    status_reason        text,
    process_instance_id  text,
    predecessor_id       uuid         REFERENCES campus.governance_config_version (id),
    effective_at         timestamptz,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX governance_config_version_no ON campus.governance_config_version (tenant_id, institution_id, key, version_no);
CREATE UNIQUE INDEX governance_config_effective ON campus.governance_config_version (tenant_id, institution_id, key)
    WHERE status = 'effective';
CREATE TRIGGER governance_config_no_delete BEFORE DELETE ON campus.governance_config_version
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- Governance evidence (G-002 / G-015): built from a closed case, published once to accreditation.
CREATE TABLE campus.governance_evidence (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    case_id              uuid         NOT NULL REFERENCES campus.governance_case (id),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    summary              text         CHECK (summary IS NULL OR length(summary) <= 4000),
    -- Campus operations figures at the time of creation (G-002) and the case outcome.
    facts                jsonb        NOT NULL DEFAULT '{}'::jsonb,
    status               text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    published_at         timestamptz,
    published_by         text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK ((status = 'published') = (published_at IS NOT NULL))
);
CREATE INDEX governance_evidence_inst ON campus.governance_evidence (tenant_id, institution_id, status);
CREATE TRIGGER governance_evidence_no_delete BEFORE DELETE ON campus.governance_evidence
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

GRANT SELECT, INSERT, UPDATE, DELETE ON campus.governance_case, campus.governance_item,
    campus.governance_config_version, campus.governance_evidence TO sutra_campus;
