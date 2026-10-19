-- Batch 8 slice 8B — hostel (L2-G03; L3-G-078 … 082, 087 … 092). docs/batch8-contracts.md §3.
--
-- Residents are (person_kind, person_id) = Domain C student / Domain E staff (plain uuids, no
-- cross-schema FK); their number / name / login subject are copied at creation for display and
-- inbox payloads. Rooms are `hostel_room` spaces of the foundation infrastructure table
-- (FK allowed: foundation table of the same schema). Charges are Domain F ids (plain uuid).
-- Money NUMERIC(14,2) (rust_decimal; strings on the wire).

-- ------------------------------------------------------------------ policy versions (G-092)
CREATE TABLE campus.hostel_policy (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    name                 text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    fee_per_term         numeric(14,2) NOT NULL CHECK (fee_per_term >= 0),
    deposit              numeric(14,2) NOT NULL DEFAULT 0 CHECK (deposit >= 0),
    curfew_time          text          CHECK (curfew_time IS NULL OR curfew_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    eligibility_note     text          CHECK (eligibility_note IS NULL OR length(eligibility_note) <= 2000),
    max_stay_terms       integer       CHECK (max_stay_terms IS NULL OR max_stay_terms BETWEEN 1 AND 40),
    effective_from       date          NOT NULL,
    status               text          NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'approved', 'active', 'superseded')),
    status_reason        text,
    process_instance_id  text,
    activated_at         timestamptz,
    superseded_at        timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX hostel_policy_inst ON campus.hostel_policy (tenant_id, institution_id, status);
-- One active policy per institution.
CREATE UNIQUE INDEX hostel_policy_one_active ON campus.hostel_policy (tenant_id, institution_id)
    WHERE status = 'active';

-- ------------------------------------------------------------------ beds of hostel rooms
CREATE TABLE campus.hostel_bed (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    room_node_id         uuid          NOT NULL REFERENCES campus.infrastructure_node (id),
    bed_code             text          NOT NULL CHECK (bed_code ~ '^[A-Z0-9][A-Z0-9_-]{0,15}$'),
    status               text          NOT NULL DEFAULT 'available'
        CHECK (status IN ('available', 'occupied', 'out_of_service')),
    status_reason        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX hostel_bed_code ON campus.hostel_bed (tenant_id, room_node_id, bed_code);
CREATE INDEX hostel_bed_inst ON campus.hostel_bed (tenant_id, institution_id, status);

-- ------------------------------------------------------------------ admission applications (G-078 / 080)
CREATE TABLE campus.hostel_application (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    number               text          NOT NULL CHECK (number ~ '^HAP-[0-9]{6,}$'),
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    person_number        text          NOT NULL,
    person_name          text          NOT NULL,
    subject_ref          text,
    policy_id            uuid          NOT NULL REFERENCES campus.hostel_policy (id),
    preference           text          CHECK (preference IS NULL OR length(preference) <= 500),
    channel              text          NOT NULL CHECK (channel IN ('self_service', 'office')),
    status               text          NOT NULL DEFAULT 'submitted'
        CHECK (status IN ('submitted', 'under_review', 'approved', 'waitlisted', 'rejected', 'withdrawn', 'allocated')),
    status_reason        text,
    process_instance_id  text,
    waitlisted_at        timestamptz,
    decided_at           timestamptz,
    allocation_id        uuid,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX hostel_application_number ON campus.hostel_application (tenant_id, institution_id, number);
CREATE INDEX hostel_application_status ON campus.hostel_application (tenant_id, institution_id, status, waitlisted_at);
-- One open application per person and institution.
CREATE UNIQUE INDEX hostel_application_one_open ON campus.hostel_application (tenant_id, institution_id, person_kind, person_id)
    WHERE status IN ('submitted', 'under_review', 'approved', 'waitlisted');

-- ------------------------------------------------------------------ allocations = residency (G-079 / 081)
CREATE TABLE campus.hostel_allocation (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    application_id       uuid          NOT NULL REFERENCES campus.hostel_application (id),
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    person_number        text          NOT NULL,
    person_name          text          NOT NULL,
    subject_ref          text,
    policy_id            uuid          NOT NULL REFERENCES campus.hostel_policy (id),
    bed_id               uuid          NOT NULL REFERENCES campus.hostel_bed (id),
    room_node_id         uuid          NOT NULL REFERENCES campus.infrastructure_node (id),
    start_date           date          NOT NULL,
    end_date             date,
    status               text          NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'on_leave', 'transfer_pending', 'vacating', 'closed')),
    status_reason        text,
    fee_amount           numeric(14,2) NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    charge_id            uuid,
    charge_number        text,
    closed_at            timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX hostel_allocation_inst ON campus.hostel_allocation (tenant_id, institution_id, status);
CREATE INDEX hostel_allocation_person ON campus.hostel_allocation (tenant_id, person_kind, person_id);
-- One live residency per person (any institution of the tenant) and one per bed.
CREATE UNIQUE INDEX hostel_allocation_one_live_person ON campus.hostel_allocation (tenant_id, person_kind, person_id)
    WHERE status <> 'closed';
CREATE UNIQUE INDEX hostel_allocation_one_live_bed ON campus.hostel_allocation (tenant_id, bed_id)
    WHERE status <> 'closed';

-- ------------------------------------------------------------------ resident leave (G-082)
CREATE TABLE campus.resident_leave (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    allocation_id        uuid          NOT NULL REFERENCES campus.hostel_allocation (id),
    from_date            date          NOT NULL,
    to_date              date          NOT NULL,
    destination          text          CHECK (destination IS NULL OR length(destination) <= 200),
    reason               text          CHECK (reason IS NULL OR length(reason) <= 1000),
    status               text          NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'rejected', 'out', 'returned', 'overdue')),
    status_reason        text,
    process_instance_id  text,
    departed_at          timestamptz,
    returned_at          timestamptz,
    returned_late        boolean       NOT NULL DEFAULT false,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (to_date >= from_date)
);
CREATE INDEX resident_leave_alloc ON campus.resident_leave (tenant_id, allocation_id, status);
CREATE INDEX resident_leave_inst ON campus.resident_leave (tenant_id, institution_id, status);

-- ------------------------------------------------------------------ room transfers (G-087)
CREATE TABLE campus.room_transfer (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    allocation_id        uuid          NOT NULL REFERENCES campus.hostel_allocation (id),
    from_bed_id          uuid          NOT NULL REFERENCES campus.hostel_bed (id),
    to_bed_id            uuid          NOT NULL REFERENCES campus.hostel_bed (id),
    reason               text          CHECK (reason IS NULL OR length(reason) <= 1000),
    status               text          NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'approved', 'rejected', 'completed')),
    status_reason        text,
    process_instance_id  text,
    completed_at         timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (from_bed_id <> to_bed_id)
);
CREATE INDEX room_transfer_alloc ON campus.room_transfer (tenant_id, allocation_id, status);

-- ------------------------------------------------------------------ room inspections (G-089)
CREATE TABLE campus.room_inspection (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    room_node_id         uuid          NOT NULL REFERENCES campus.infrastructure_node (id),
    allocation_id        uuid          REFERENCES campus.hostel_allocation (id),
    scheduled_on         date          NOT NULL,
    status               text          NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'completed', 'damage_found', 'charged', 'closed')),
    status_reason        text,
    findings             text          CHECK (findings IS NULL OR length(findings) <= 4000),
    damage_amount        numeric(14,2) CHECK (damage_amount IS NULL OR damage_amount >= 0),
    charge_id            uuid,
    charge_number        text,
    completed_at         timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX room_inspection_room ON campus.room_inspection (tenant_id, room_node_id, status);
CREATE INDEX room_inspection_alloc ON campus.room_inspection (tenant_id, allocation_id, status);

-- ------------------------------------------------------------------ mess (G-090 / 091)
CREATE TABLE campus.mess (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    code                 text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{0,31}$'),
    name                 text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    menu_type            text          NOT NULL CHECK (menu_type IN ('veg', 'non_veg', 'mixed')),
    capacity             integer       NOT NULL CHECK (capacity >= 1),
    status               text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    status_reason        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX mess_code ON campus.mess (tenant_id, institution_id, code);

CREATE TABLE campus.mess_allocation (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    mess_id              uuid          NOT NULL REFERENCES campus.mess (id),
    hostel_allocation_id uuid          NOT NULL REFERENCES campus.hostel_allocation (id),
    person_number        text          NOT NULL,
    person_name          text          NOT NULL,
    start_date           date          NOT NULL,
    end_date             date,
    status               text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'changed', 'ended')),
    status_reason        text,
    previous_id          uuid          REFERENCES campus.mess_allocation (id),
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX mess_allocation_mess ON campus.mess_allocation (tenant_id, mess_id, status);
CREATE UNIQUE INDEX mess_allocation_one_active ON campus.mess_allocation (tenant_id, hostel_allocation_id)
    WHERE status = 'active';

-- Hostel rows are never physically deleted.
CREATE TRIGGER hostel_policy_no_delete BEFORE DELETE ON campus.hostel_policy
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER hostel_application_no_delete BEFORE DELETE ON campus.hostel_application
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER hostel_allocation_no_delete BEFORE DELETE ON campus.hostel_allocation
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
