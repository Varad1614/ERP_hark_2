-- Batch 8 · slice 8C — Domain H dashboards (H07; L3-H-048, 050, 052, 054, F-076) and regulatory
-- reporting (H08; L3-H-055…060). Read-only projections: every figure comes from the
-- OperationalMetricsPort adapters of the owning domains (never cross-schema SQL); snapshot values
-- are frozen copies.

CREATE TABLE services.widget_definition (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    code            text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    title           text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- `<domain>.<key>` of an OperationalMetricsPort figure (validated against the live catalogue).
    metric_key      text         NOT NULL CHECK (metric_key ~ '^[a-z][a-z0-9_]*\.[a-z][a-z0-9_.]*$'),
    visualization   text         NOT NULL DEFAULT 'stat' CHECK (visualization IN ('stat', 'gauge', 'table')),
    description     text,
    status          text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'retired')),
    status_reason   text,
    version         bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX widget_definition_code ON services.widget_definition (tenant_id, institution_id, code);

CREATE TABLE services.dashboard (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    -- The dashboard requirement (L3-H-048): purpose / audience as stated by the requester.
    purpose              text         NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 2000),
    -- `finance` dashboards (F-076) may only bind `finance.*` figures.
    category             text         NOT NULL DEFAULT 'operational'
        CHECK (category IN ('operational', 'finance', 'academic', 'campus', 'people', 'regulatory')),
    status               text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'in_review', 'published', 'retired')),
    status_reason        text,
    current_version_id   uuid,
    process_instance_id  text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX dashboard_code ON services.dashboard (tenant_id, institution_id, code);

CREATE TABLE services.dashboard_version (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    dashboard_id         uuid         NOT NULL REFERENCES services.dashboard (id),
    version_no           integer      NOT NULL CHECK (version_no >= 1),
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'in_review', 'approved', 'published', 'superseded', 'rejected')),
    change_note          text,
    -- Frozen widget layout: [{widgetId, code, title, metricKey, visualization, position}].
    widgets              jsonb        NOT NULL DEFAULT '[]'::jsonb,
    status_reason        text,
    process_instance_id  text,
    published_at         timestamptz,
    published_by         text,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (dashboard_id, version_no)
);
CREATE UNIQUE INDEX dashboard_version_one_published ON services.dashboard_version (dashboard_id) WHERE status = 'published';
CREATE UNIQUE INDEX dashboard_version_one_open ON services.dashboard_version (dashboard_id)
    WHERE status IN ('draft', 'in_review', 'approved');
ALTER TABLE services.dashboard
    ADD CONSTRAINT dashboard_current_version_fk FOREIGN KEY (current_version_id) REFERENCES services.dashboard_version (id);

CREATE TABLE services.dashboard_assignment (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    dashboard_id    uuid         NOT NULL REFERENCES services.dashboard (id),
    role            text         NOT NULL CHECK (role IN ('administrator', 'faculty', 'student')),
    -- Optional Domain B organisation unit context (plain uuid).
    org_unit_id     uuid,
    status          text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    version         bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX dashboard_assignment_live ON services.dashboard_assignment
    (dashboard_id, role, COALESCE(org_unit_id, '00000000-0000-0000-0000-000000000000'::uuid)) WHERE status = 'active';
CREATE INDEX dashboard_assignment_role ON services.dashboard_assignment (tenant_id, role, status);

CREATE TABLE services.reporting_framework (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    regulator            text         NOT NULL CHECK (length(btrim(regulator)) BETWEEN 1 AND 100),
    version_no           integer      NOT NULL CHECK (version_no >= 1),
    -- Data definition: the operational figures and manual fields the return needs.
    metric_keys          text[]       NOT NULL DEFAULT '{}',
    manual_fields        jsonb        NOT NULL DEFAULT '[]'::jsonb,
    status               text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'superseded')),
    approval_state       text         NOT NULL DEFAULT 'none' CHECK (approval_state IN ('none', 'pending', 'approved', 'rejected')),
    status_reason        text,
    process_instance_id  text,
    predecessor_id       uuid         REFERENCES services.reporting_framework (id),
    activated_at         timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code, version_no)
);
CREATE UNIQUE INDEX reporting_framework_one_active ON services.reporting_framework (tenant_id, institution_id, code)
    WHERE status = 'active';

CREATE TABLE services.reporting_cycle (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    framework_id     uuid         NOT NULL REFERENCES services.reporting_framework (id),
    number           text         NOT NULL CHECK (number ~ '^RC-[0-9]{6,}$'),
    period_label     text         NOT NULL CHECK (length(btrim(period_label)) BETWEEN 1 AND 40),
    due_on           date,
    -- Collection plan frozen from the framework version: {metricKeys[], manualFields[]}.
    collection_plan  jsonb        NOT NULL,
    status           text         NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'collecting', 'validating', 'packaged', 'submitted', 'closed')),
    status_reason    text,
    version          bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX reporting_cycle_number ON services.reporting_cycle (tenant_id, institution_id, number);
CREATE UNIQUE INDEX reporting_cycle_period ON services.reporting_cycle (framework_id, period_label);

CREATE TABLE services.report_snapshot (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    cycle_id         uuid         NOT NULL REFERENCES services.reporting_cycle (id),
    seq              integer      NOT NULL CHECK (seq >= 1),
    status           text         NOT NULL DEFAULT 'collected' CHECK (status IN ('collected', 'validated', 'rejected')),
    -- Validation issues found at collection: [{key, issue}].
    issues           jsonb        NOT NULL DEFAULT '[]'::jsonb,
    status_reason    text,
    collected_by     text,
    version          bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (cycle_id, seq)
);

CREATE TABLE services.report_snapshot_value (
    id            uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id     uuid          NOT NULL,
    snapshot_id   uuid          NOT NULL REFERENCES services.report_snapshot (id),
    key           text          NOT NULL,
    label         text          NOT NULL,
    source        text          NOT NULL CHECK (source IN ('metric', 'manual')),
    -- Metric figures keep their decimal; manual fields keep the text as entered.
    value         numeric(20,4),
    value_text    text,
    unit          text,
    created_at    timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (snapshot_id, key)
);
-- Frozen: snapshot values are never rewritten (a correction is a new snapshot).
CREATE TRIGGER report_snapshot_value_frozen BEFORE UPDATE OR DELETE ON services.report_snapshot_value
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

CREATE TABLE services.submission_package (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    cycle_id             uuid         NOT NULL REFERENCES services.reporting_cycle (id),
    snapshot_id          uuid         NOT NULL REFERENCES services.report_snapshot (id),
    number               text         NOT NULL CHECK (number ~ '^SUB-[0-9]{6,}$'),
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'submitted', 'acknowledged', 'corrected')),
    status_reason        text,
    process_instance_id  text,
    approved_by          text,
    submitted_at         timestamptz,
    acknowledgement_ref  text,
    acknowledged_at      timestamptz,
    corrected_from_id    uuid         REFERENCES services.submission_package (id),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX submission_package_number ON services.submission_package (tenant_id, institution_id, number);
CREATE UNIQUE INDEX submission_package_one_open ON services.submission_package (cycle_id)
    WHERE status IN ('draft', 'pending_approval', 'approved', 'submitted', 'acknowledged');

CREATE TRIGGER dashboard_no_delete BEFORE DELETE ON services.dashboard
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();
CREATE TRIGGER dashboard_version_no_delete BEFORE DELETE ON services.dashboard_version
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();
CREATE TRIGGER reporting_framework_no_delete BEFORE DELETE ON services.reporting_framework
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();
CREATE TRIGGER report_snapshot_no_delete BEFORE DELETE ON services.report_snapshot
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();
CREATE TRIGGER submission_package_no_delete BEFORE DELETE ON services.submission_package
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

-- Runtime grants (restated; default privileges cover new tables as well).
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA services TO sutra_services;
REVOKE UPDATE, DELETE, TRUNCATE ON services.status_history FROM sutra_services;
REVOKE UPDATE, DELETE, TRUNCATE ON services.report_snapshot_value FROM sutra_services;
