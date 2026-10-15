-- Batch 9 slice 9E — accreditation (Domain H09; L3-H-061…071; docs/batch9-contracts.md §12.2).
--
-- Versioned accreditation frameworks (NAAC / NBA / NIRF criteria), accreditation cycles with an
-- approved assessment plan, and one governed record table for obligations, evidence, internal
-- audits, external assessments, findings (CAPA), quality improvements, readiness decisions,
-- periodic compliance reviews and self-study reports, with child items (requirements,
-- checklists, CAPA / improvement actions). Figures come only through the metrics port; campus
-- governance evidence is cited by id (published event), never read by SQL.

CREATE TABLE services.accreditation_framework (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    body                 text         NOT NULL CHECK (body IN ('NAAC', 'NBA', 'NIRF', 'OTHER')),
    version_no           integer      NOT NULL CHECK (version_no >= 1),
    -- [{code, title, weight, metricKeys[]}]
    criteria             jsonb        NOT NULL,
    status               text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'superseded')),
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    status_reason        text,
    process_instance_id  text,
    predecessor_id       uuid         REFERENCES services.accreditation_framework (id),
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX accreditation_framework_version ON services.accreditation_framework (tenant_id, institution_id, code, version_no);
CREATE UNIQUE INDEX accreditation_framework_active ON services.accreditation_framework (tenant_id, institution_id, code)
    WHERE status = 'active';
CREATE TRIGGER accreditation_framework_no_delete BEFORE DELETE ON services.accreditation_framework
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

CREATE TABLE services.accreditation_cycle (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    framework_id         uuid         NOT NULL REFERENCES services.accreditation_framework (id),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    period_start         date         NOT NULL,
    period_end           date         NOT NULL,
    -- Assessment plan (H-062): [{criterionCode, owner, dueOn}]
    plan                 jsonb        NOT NULL DEFAULT '[]'::jsonb,
    status               text         NOT NULL DEFAULT 'planning'
        CHECK (status IN ('planning', 'active', 'submitted', 'closed')),
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    status_reason        text,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start)
);
CREATE UNIQUE INDEX accreditation_cycle_number ON services.accreditation_cycle (tenant_id, institution_id, number);
CREATE TRIGGER accreditation_cycle_no_delete BEFORE DELETE ON services.accreditation_cycle
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

CREATE TABLE services.accreditation_record (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    kind                 text         NOT NULL CHECK (kind IN ('obligation', 'evidence', 'internal_audit',
                                          'external_assessment', 'finding', 'improvement', 'readiness',
                                          'compliance_review', 'self_study')),
    cycle_id             uuid         REFERENCES services.accreditation_cycle (id),
    criterion_code       text,
    parent_id            uuid         REFERENCES services.accreditation_record (id),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description          text         CHECK (description IS NULL OR length(description) <= 4000),
    severity             text         NOT NULL DEFAULT 'medium' CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    owner                text,
    due_on               date,
    -- Improvement (H-068): figure tracked through the metrics port and the better direction.
    metric_key           text,
    better               text         CHECK (better IS NULL OR better IN ('higher', 'lower')),
    -- Citations: {regulatorySnapshotIds[], governanceEvidenceIds[], sourceUrl}
    refs                 jsonb        NOT NULL DEFAULT '{}'::jsonb,
    facts                jsonb        NOT NULL DEFAULT '{}'::jsonb,
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'open', 'pending_decision', 'closed', 'cancelled')),
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    outcome              text,
    rationale            text         CHECK (rationale IS NULL OR length(rationale) <= 4000),
    status_reason        text,
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    handed_off_at        timestamptz,
    handoff_reference    text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (kind NOT IN ('evidence', 'readiness', 'self_study', 'internal_audit', 'external_assessment') OR cycle_id IS NOT NULL),
    CHECK (kind <> 'improvement' OR (metric_key IS NOT NULL AND better IS NOT NULL)),
    CHECK (handed_off_at IS NULL OR kind = 'self_study')
);
CREATE UNIQUE INDEX accreditation_record_number ON services.accreditation_record (tenant_id, institution_id, number);
CREATE INDEX accreditation_record_inst ON services.accreditation_record (tenant_id, institution_id, kind, status);
CREATE INDEX accreditation_record_cycle ON services.accreditation_record (tenant_id, cycle_id, kind);
CREATE TRIGGER accreditation_record_no_delete BEFORE DELETE ON services.accreditation_record
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

CREATE TABLE services.accreditation_item (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    record_id            uuid         NOT NULL REFERENCES services.accreditation_record (id),
    kind                 text         NOT NULL CHECK (kind IN ('requirement', 'checklist', 'action')),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    owner                text,
    due_on               date,
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
CREATE INDEX accreditation_item_record ON services.accreditation_item (tenant_id, record_id, created_at);
CREATE TRIGGER accreditation_item_no_delete BEFORE DELETE ON services.accreditation_item
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

GRANT SELECT, INSERT, UPDATE, DELETE ON services.accreditation_framework, services.accreditation_cycle,
    services.accreditation_record, services.accreditation_item TO sutra_services;
