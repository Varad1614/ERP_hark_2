-- Batch 8 foundation — Domain G (Campus operations) shared tables (docs/batch8-contracts.md §3).
--
-- Foundation-owned. Slices add their own tables in their migration ranges (8A 20261115000010-029,
-- 8B …030-049, 8C …050-069; campus tables named campus.<what>) and never edit this file.
--
-- Money is NUMERIC(14,2) (rust_decimal in Rust — never floats); on the wire JSON strings.
-- Domain B/C/E/F ids (campus_id, student_id, staff_id, charge_id) are plain uuids — no cross-schema FKs.
--
-- Write rules (§3.3):
--   infrastructure_node  8A; rows inserted ONLY via `shared::infrastructure::insert_node`; status ONLY via
--                        `shared::infrastructure::change_node_status` (8B/8C read; tests seed via support::b8).
--   service_ticket       opened by ANY slice ONLY via `shared::tickets::open_ticket`; status ONLY via
--                        `shared::tickets::change_ticket_status` (8A).
--   number_sequence      `shared::numbering::next` only.
--   status_history       `shared::lifecycle::record_history` only (append-only).

-- Append-only guard reused by every campus history / ledger table (slices reuse it too).
CREATE FUNCTION campus.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of campus.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- ------------------------------------------------------------------ infrastructure hierarchy (L2-G10)
CREATE TABLE campus.infrastructure_node (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    -- Domain B campus (plain uuid, validated through InstitutionReferencePort).
    campus_id            uuid         NOT NULL,
    parent_id            uuid         REFERENCES campus.infrastructure_node (id),
    node_type            text         NOT NULL CHECK (node_type IN ('site', 'building', 'floor', 'space', 'zone')),
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_.-]{0,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    space_type           text         CHECK (space_type IS NULL OR space_type IN ('classroom', 'lab', 'seminar_room',
                                          'auditorium', 'office', 'hostel_room', 'library', 'reading_room', 'store',
                                          'workshop', 'sports', 'other')),
    -- Seats / beds / persons (spaces); NULL = not applicable.
    capacity             integer      CHECK (capacity IS NULL OR capacity >= 0),
    floor_number         integer,
    area_sq_m            numeric(10,2) CHECK (area_sq_m IS NULL OR area_sq_m >= 0),
    -- Free attributes (capabilities live in 8A tables): accessibility, notes …
    attributes           jsonb        NOT NULL DEFAULT '{}'::jsonb,
    status               text         NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'active', 'suspended', 'decommissioned', 'archived')),
    status_reason        text,
    status_changed_at    timestamptz  NOT NULL DEFAULT now(),
    effective_from       date,
    process_instance_id  text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK ((node_type = 'space') = (space_type IS NOT NULL)),
    CHECK ((node_type = 'site') = (parent_id IS NULL)),
    CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE UNIQUE INDEX infrastructure_node_code
    ON campus.infrastructure_node (tenant_id, institution_id, code);
CREATE INDEX infrastructure_node_parent ON campus.infrastructure_node (tenant_id, parent_id);
CREATE INDEX infrastructure_node_campus ON campus.infrastructure_node (tenant_id, campus_id, node_type, status);
CREATE TRIGGER infrastructure_node_no_delete BEFORE DELETE ON campus.infrastructure_node
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- ------------------------------------------------------------------ service desk tickets (L2-G01 …)
CREATE TABLE campus.service_ticket (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    category             text         NOT NULL CHECK (category IN ('facility', 'maintenance', 'hostel', 'transport',
                                          'library', 'laboratory', 'it', 'other')),
    source               text         NOT NULL CHECK (source IN ('self_service', 'admin', 'hostel', 'transport',
                                          'library', 'laboratory', 'preventive')),
    -- The slice aggregate behind the ticket (hostel allocation, route, copy …), plain uuid.
    source_ref_id        uuid,
    -- Complaint (true) or service request (false) (L3-G-016 vs 017).
    is_complaint         boolean      NOT NULL DEFAULT false,
    node_id              uuid         REFERENCES campus.infrastructure_node (id),
    requester_kind       text         CHECK (requester_kind IS NULL OR requester_kind IN ('student', 'staff')),
    requester_id         uuid,
    requester_subject    text,
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    description          text         CHECK (description IS NULL OR length(description) <= 4000),
    priority             text         NOT NULL DEFAULT 'medium' CHECK (priority IN ('low', 'medium', 'high', 'critical')),
    status               text         NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'triaged', 'assigned', 'in_progress', 'on_hold', 'resolved', 'closed',
                          'reopened', 'cancelled', 'merged')),
    status_reason        text,
    status_changed_at    timestamptz  NOT NULL DEFAULT now(),
    -- Technician / team (login subject or team code; 8A).
    assignee             text,
    sla_due_at           timestamptz  NOT NULL,
    sla_breached         boolean      NOT NULL DEFAULT false,
    escalation_level     integer      NOT NULL DEFAULT 0 CHECK (escalation_level >= 0),
    resolved_at          timestamptz,
    closed_at            timestamptz,
    resolution           text,
    canonical_ticket_id  uuid         REFERENCES campus.service_ticket (id),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (canonical_ticket_id IS NULL OR canonical_ticket_id <> id),
    CHECK ((requester_kind IS NULL) = (requester_id IS NULL))
);
CREATE UNIQUE INDEX service_ticket_number ON campus.service_ticket (tenant_id, institution_id, number);
CREATE INDEX service_ticket_status ON campus.service_ticket (tenant_id, institution_id, status, priority);
CREATE INDEX service_ticket_requester ON campus.service_ticket (tenant_id, requester_subject);
CREATE INDEX service_ticket_source ON campus.service_ticket (tenant_id, source, source_ref_id);
CREATE TRIGGER service_ticket_no_delete BEFORE DELETE ON campus.service_ticket
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- ------------------------------------------------------------------ numbering + history
CREATE TABLE campus.number_sequence (
    tenant_id        uuid    NOT NULL,
    institution_id   uuid    NOT NULL,
    kind             text    NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,47}$'),
    next_value       bigint  NOT NULL CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

CREATE TABLE campus.status_history (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    aggregate_type       text         NOT NULL,
    aggregate_id         uuid         NOT NULL,
    from_status          text,
    to_status            text         NOT NULL,
    reason               text,
    cause_event_type     text,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX campus_status_history_aggregate_idx
    ON campus.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER campus_status_history_append_only BEFORE UPDATE OR DELETE ON campus.status_history
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

-- ------------------------------------------------------------------ runtime role (sutra_campus)
GRANT USAGE ON SCHEMA campus TO sutra_campus;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA campus TO sutra_campus;
REVOKE UPDATE, DELETE, TRUNCATE ON campus.status_history FROM sutra_campus;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_campus;
