-- Batch 8 slice 8A — work orders and preventive maintenance (L2-G01; L3-G-020…024, 026).

-- ------------------------------------------------------------------ preventive plans
CREATE TABLE campus.maintenance_plan (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    node_id              uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description          text,
    frequency            text         NOT NULL CHECK (frequency IN ('weekly', 'monthly', 'quarterly', 'half_yearly', 'yearly')),
    start_date           date         NOT NULL,
    end_date             date,
    default_assignee     text,
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'active', 'suspended', 'retired')),
    -- Approval of the draft (MAINTENANCE_PLAN_APPROVAL): not_requested → pending → approved / rejected.
    approval_state       text         NOT NULL DEFAULT 'not_requested'
        CHECK (approval_state IN ('not_requested', 'pending', 'approved', 'rejected')),
    status_reason        text,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= start_date)
);
CREATE INDEX maintenance_plan_inst ON campus.maintenance_plan (tenant_id, institution_id, status);

-- ------------------------------------------------------------------ work orders
CREATE TABLE campus.work_order (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    kind                 text         NOT NULL CHECK (kind IN ('corrective', 'preventive')),
    ticket_id            uuid         REFERENCES campus.service_ticket (id),
    plan_id              uuid         REFERENCES campus.maintenance_plan (id),
    node_id              uuid         REFERENCES campus.infrastructure_node (id),
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description          text,
    assignee             text,
    scheduled_for        date,
    status               text         NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'assigned', 'in_progress', 'on_hold', 'completed', 'verified', 'reopened', 'cancelled')),
    status_reason        text,
    started_at           timestamptz,
    completed_at         timestamptz,
    verified_at          timestamptz,
    completion_notes     text,
    verification_notes   text,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (kind = 'preventive' OR ticket_id IS NOT NULL)
);
CREATE UNIQUE INDEX work_order_number ON campus.work_order (tenant_id, institution_id, number);
CREATE INDEX work_order_ticket ON campus.work_order (tenant_id, ticket_id);
CREATE INDEX work_order_inst ON campus.work_order (tenant_id, institution_id, status);
CREATE TRIGGER work_order_no_delete BEFORE DELETE ON campus.work_order
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- Technician assignment / reassignment history (append-only).
CREATE TABLE campus.work_order_assignment (
    id                 uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid         NOT NULL,
    work_order_id      uuid         NOT NULL REFERENCES campus.work_order (id),
    assignee           text         NOT NULL,
    previous_assignee  text,
    reason             text,
    actor              text,
    created_at         timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX work_order_assignment_wo ON campus.work_order_assignment (tenant_id, work_order_id, created_at);
REVOKE UPDATE, DELETE, TRUNCATE ON campus.work_order_assignment FROM sutra_campus;
CREATE TRIGGER work_order_assignment_append_only BEFORE UPDATE OR DELETE ON campus.work_order_assignment
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- ------------------------------------------------------------------ maintenance cycles
CREATE TABLE campus.maintenance_cycle (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    plan_id              uuid         NOT NULL REFERENCES campus.maintenance_plan (id),
    period_start         date         NOT NULL,
    due_on               date         NOT NULL,
    status               text         NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'due', 'deferred', 'completed', 'missed')),
    status_reason        text,
    work_order_id        uuid         REFERENCES campus.work_order (id),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
-- One cycle per plan and period: generation is idempotent.
CREATE UNIQUE INDEX maintenance_cycle_period ON campus.maintenance_cycle (tenant_id, plan_id, period_start);
