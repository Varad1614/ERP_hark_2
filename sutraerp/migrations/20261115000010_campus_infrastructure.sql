-- Batch 8 slice 8A — infrastructure capabilities, availability profiles and decommissioning
-- requests (L2-G10; L3-G-278, 281…290, 315). docs/batch8-contracts.md §3.
--
-- Nodes themselves live in the foundation table campus.infrastructure_node (inserted only via
-- shared::infrastructure::insert_node, status only via change_node_status). These tables hang off it.

-- ------------------------------------------------------------------ capabilities (L3-G-287)
CREATE TABLE campus.space_capability (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    node_id         uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    code            text         NOT NULL CHECK (code ~ '^[a-z][a-z0-9_]{0,47}$'),
    label           text         NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    quantity        integer      CHECK (quantity IS NULL OR quantity >= 0),
    created_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX space_capability_code ON campus.space_capability (tenant_id, node_id, code);

-- ------------------------------------------------------------------ availability profile (L3-G-289 / 315)
-- One per node. Weekly open hours as JSON [{weekday 1..7, opensAt "HH:MM", closesAt "HH:MM"}]
-- in the institution's business timezone; blackout dates; bookable flag.
CREATE TABLE campus.availability_profile (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    node_id         uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    bookable        boolean      NOT NULL DEFAULT true,
    weekly_hours    jsonb        NOT NULL DEFAULT '[]'::jsonb,
    blackout_dates  date[]       NOT NULL DEFAULT '{}',
    notes           text,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX availability_profile_node ON campus.availability_profile (tenant_id, node_id);

-- ------------------------------------------------------------------ decommissioning (NODE_DECOMMISSION)
CREATE TABLE campus.infrastructure_decommission (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    node_id              uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    status               text         NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected')),
    reason               text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 2000),
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    decision_reason      text,
    decided_at           timestamptz,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX infrastructure_decommission_pending
    ON campus.infrastructure_decommission (tenant_id, node_id) WHERE status = 'pending';
