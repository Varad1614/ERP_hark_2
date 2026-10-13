-- Batch 5 slice 5A: governed finance configuration and financial rule versions with rollback
-- (L3-F-084 / F-085 + L2-F00 amendments 2026-09-01). A rollback never reactivates a historical
-- version: it creates a new version based on an eligible prior one, which goes through the same
-- forward lifecycle (draft → in_review → effective).
CREATE TABLE finance.finance_config_version (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    version_no           integer       NOT NULL CHECK (version_no > 0),
    settings             jsonb         NOT NULL CHECK (jsonb_typeof(settings) = 'object'),
    change_note          text,
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'in_review',
                                           'effective', 'superseded', 'rolled_back', 'retired')),
    status_reason        text,
    based_on_id          uuid          REFERENCES finance.finance_config_version (id),
    rollback_reason      text,
    process_instance_id  text,
    approved_at          timestamptz,
    approved_by          text,
    effective_from       timestamptz,
    effective_until      timestamptz,
    created_by           text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (rollback_reason IS NULL OR based_on_id IS NOT NULL)
);
CREATE UNIQUE INDEX finance_config_version_no
    ON finance.finance_config_version (tenant_id, institution_id, version_no);
CREATE UNIQUE INDEX finance_config_one_effective
    ON finance.finance_config_version (tenant_id, institution_id) WHERE status = 'effective';

CREATE TABLE finance.finance_rule (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    rule_key             text          NOT NULL CHECK (rule_key IN ('late_fee', 'rounding',
                                           'instalment_policy', 'refund_window', 'write_off_threshold')),
    version_no           integer       NOT NULL CHECK (version_no > 0),
    parameters           jsonb         NOT NULL CHECK (jsonb_typeof(parameters) = 'object'),
    change_note          text,
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'in_review',
                                           'effective', 'superseded', 'rolled_back', 'retired')),
    status_reason        text,
    based_on_id          uuid          REFERENCES finance.finance_rule (id),
    rollback_reason      text,
    process_instance_id  text,
    approved_at          timestamptz,
    approved_by          text,
    effective_from       timestamptz,
    effective_until      timestamptz,
    created_by           text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (rollback_reason IS NULL OR based_on_id IS NOT NULL)
);
CREATE UNIQUE INDEX finance_rule_version_no
    ON finance.finance_rule (tenant_id, institution_id, rule_key, version_no);
CREATE UNIQUE INDEX finance_rule_one_effective
    ON finance.finance_rule (tenant_id, institution_id, rule_key) WHERE status = 'effective';
