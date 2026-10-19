-- Batch 8 slice 8B — transport (L2-G04; L3-G-093 … 097, 099 … 102, 104 … 106, 108, 109, 113 … 116).
-- docs/batch8-contracts.md §3.
--
-- Passengers and drivers are (person_kind, person_id) / Domain E staff ids (plain uuids, no
-- cross-schema FK). A route keeps effective-dated versions with ordered stops: a modification is a
-- new version, an older version is superseded (effective_to set), never rewritten.
-- Money NUMERIC(14,2) (rust_decimal; strings on the wire).

-- ------------------------------------------------------------------ stops (G-096 / 097)
CREATE TABLE campus.transport_stop (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    code                 text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{0,31}$'),
    name                 text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    landmark             text          CHECK (landmark IS NULL OR length(landmark) <= 300),
    node_id              uuid          REFERENCES campus.infrastructure_node (id),
    status               text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended', 'retired')),
    status_reason        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX transport_stop_code ON campus.transport_stop (tenant_id, institution_id, code);

-- ------------------------------------------------------------------ routes + versions (G-093 … 095)
CREATE TABLE campus.transport_route (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    code                 text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{0,31}$'),
    name                 text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'suspended', 'retired')),
    status_reason        text,
    current_version_id   uuid,
    process_instance_id  text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX transport_route_code ON campus.transport_route (tenant_id, institution_id, code);

CREATE TABLE campus.route_version (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    route_id             uuid          NOT NULL REFERENCES campus.transport_route (id),
    version_no           integer       NOT NULL CHECK (version_no >= 1),
    effective_from       date          NOT NULL,
    effective_to         date,
    -- Seats offered on the route (passenger allocation capacity check).
    capacity             integer       NOT NULL CHECK (capacity >= 1),
    -- Transport pass fee per pass period (0 = free).
    fare                 numeric(14,2) NOT NULL DEFAULT 0 CHECK (fare >= 0),
    note                 text          CHECK (note IS NULL OR length(note) <= 1000),
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'superseded')),
    status_reason        text,
    process_instance_id  text,
    activated_at         timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_to >= effective_from - 1)
);
CREATE UNIQUE INDEX route_version_no ON campus.route_version (tenant_id, route_id, version_no);
CREATE UNIQUE INDEX route_version_one_draft ON campus.route_version (tenant_id, route_id) WHERE status = 'draft';
CREATE UNIQUE INDEX route_version_one_active ON campus.route_version (tenant_id, route_id) WHERE status = 'active';

CREATE TABLE campus.route_stop (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    route_version_id     uuid          NOT NULL REFERENCES campus.route_version (id),
    stop_id              uuid          NOT NULL REFERENCES campus.transport_stop (id),
    sequence             integer       NOT NULL CHECK (sequence >= 1),
    pickup_time          text          CHECK (pickup_time IS NULL OR pickup_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX route_stop_seq ON campus.route_stop (route_version_id, sequence);
CREATE UNIQUE INDEX route_stop_once ON campus.route_stop (route_version_id, stop_id);

-- ------------------------------------------------------------------ vehicles (G-108 / 109)
CREATE TABLE campus.vehicle (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    registration_no      text          NOT NULL CHECK (registration_no ~ '^[A-Z0-9][A-Z0-9 -]{1,19}$'),
    label                text          NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 200),
    capacity             integer       NOT NULL CHECK (capacity >= 1),
    status               text          NOT NULL DEFAULT 'available'
        CHECK (status IN ('available', 'assigned', 'under_maintenance', 'broken_down', 'retired')),
    status_reason        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX vehicle_registration ON campus.vehicle (tenant_id, institution_id, registration_no);

-- ------------------------------------------------------------------ driver assignments (G-102)
CREATE TABLE campus.driver_assignment (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    route_id             uuid          NOT NULL REFERENCES campus.transport_route (id),
    staff_id             uuid          NOT NULL,
    staff_number         text          NOT NULL,
    driver_name          text          NOT NULL,
    licence_no           text          NOT NULL CHECK (length(btrim(licence_no)) BETWEEN 3 AND 40),
    valid_from           date          NOT NULL,
    valid_to             date,
    status               text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'ended')),
    status_reason        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (valid_to IS NULL OR valid_to >= valid_from)
);
CREATE INDEX driver_assignment_route ON campus.driver_assignment (tenant_id, route_id, status);
CREATE UNIQUE INDEX driver_assignment_once ON campus.driver_assignment (tenant_id, route_id, staff_id)
    WHERE status = 'active';

-- ------------------------------------------------------------------ passenger allocations (G-099 … 101)
CREATE TABLE campus.transport_allocation (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    person_number        text          NOT NULL,
    person_name          text          NOT NULL,
    subject_ref          text,
    route_id             uuid          NOT NULL REFERENCES campus.transport_route (id),
    stop_id              uuid          NOT NULL REFERENCES campus.transport_stop (id),
    channel              text          NOT NULL CHECK (channel IN ('self_service', 'office')),
    start_date           date          NOT NULL,
    end_date             date,
    status               text          NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'active', 'changed', 'withdrawn')),
    status_reason        text,
    -- Set when the stop / route is suspended (G-095 / 097): the office must adjust.
    flagged              boolean       NOT NULL DEFAULT false,
    flag_reason          text,
    previous_id          uuid          REFERENCES campus.transport_allocation (id),
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX transport_allocation_route ON campus.transport_allocation (tenant_id, route_id, status);
CREATE INDEX transport_allocation_stop ON campus.transport_allocation (tenant_id, stop_id, status);
CREATE INDEX transport_allocation_inst ON campus.transport_allocation (tenant_id, institution_id, status);
-- One live allocation per person.
CREATE UNIQUE INDEX transport_allocation_one_live ON campus.transport_allocation (tenant_id, person_kind, person_id)
    WHERE status IN ('requested', 'active');

-- ------------------------------------------------------------------ passes (G-113 … 116)
CREATE TABLE campus.transport_pass (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    number               text          NOT NULL CHECK (number ~ '^TP-[0-9]{6,}$'),
    allocation_id        uuid          NOT NULL REFERENCES campus.transport_allocation (id),
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    person_number        text          NOT NULL,
    person_name          text          NOT NULL,
    subject_ref          text,
    route_id             uuid          NOT NULL REFERENCES campus.transport_route (id),
    valid_from           date          NOT NULL,
    valid_to             date          NOT NULL,
    kind                 text          NOT NULL CHECK (kind IN ('new', 'renewal', 'replacement')),
    status               text          NOT NULL DEFAULT 'issued'
        CHECK (status IN ('issued', 'active', 'expired', 'suspended', 'replaced', 'cancelled')),
    status_reason        text,
    fee_amount           numeric(14,2) NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    charge_id            uuid,
    charge_number        text,
    replaces_id          uuid          REFERENCES campus.transport_pass (id),
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (valid_to >= valid_from)
);
CREATE UNIQUE INDEX transport_pass_number ON campus.transport_pass (tenant_id, institution_id, number);
CREATE INDEX transport_pass_alloc ON campus.transport_pass (tenant_id, allocation_id, status);
CREATE UNIQUE INDEX transport_pass_one_live ON campus.transport_pass (tenant_id, allocation_id)
    WHERE status IN ('issued', 'active', 'suspended');

-- ------------------------------------------------------------------ trips (G-104 … 106)
CREATE TABLE campus.trip (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    route_id             uuid          NOT NULL REFERENCES campus.transport_route (id),
    route_version_id     uuid          NOT NULL REFERENCES campus.route_version (id),
    trip_date            date          NOT NULL,
    direction            text          NOT NULL CHECK (direction IN ('pickup', 'drop')),
    status               text          NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'ready', 'in_progress', 'completed', 'cancelled')),
    status_reason        text,
    vehicle_id           uuid          REFERENCES campus.vehicle (id),
    driver_assignment_id uuid          REFERENCES campus.driver_assignment (id),
    checklist            jsonb         NOT NULL DEFAULT '{}'::jsonb,
    -- The vehicle broke down: a replacement is needed before the trip can run (G-109).
    needs_vehicle        boolean       NOT NULL DEFAULT false,
    passenger_count      integer       CHECK (passenger_count IS NULL OR passenger_count >= 0),
    started_at           timestamptz,
    completed_at         timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX trip_route ON campus.trip (tenant_id, route_id, trip_date);
CREATE INDEX trip_inst ON campus.trip (tenant_id, institution_id, trip_date, status);
CREATE INDEX trip_vehicle ON campus.trip (tenant_id, vehicle_id, status);
CREATE UNIQUE INDEX trip_once ON campus.trip (tenant_id, route_id, trip_date, direction)
    WHERE status <> 'cancelled';

CREATE TRIGGER transport_route_no_delete BEFORE DELETE ON campus.transport_route
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER route_version_no_delete BEFORE DELETE ON campus.route_version
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER transport_pass_no_delete BEFORE DELETE ON campus.transport_pass
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
