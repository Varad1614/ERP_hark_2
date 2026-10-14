-- Batch 8 slice 8A — space reservations / room booking (L2-G10; L3-G-291…293, 315).
-- Times are business-local (institution timezone): a date plus start / end times.

CREATE TABLE campus.space_reservation (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    number               text         NOT NULL CHECK (number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    node_id              uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    -- Requester (Domain C student / Domain E staff), NULL for an office booking.
    requester_kind       text         CHECK (requester_kind IS NULL OR requester_kind IN ('student', 'staff')),
    requester_id         uuid,
    requester_subject    text,
    purpose              text         NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 300),
    attendees            integer      NOT NULL CHECK (attendees >= 1),
    reservation_date     date         NOT NULL,
    start_time           time         NOT NULL,
    end_time             time         NOT NULL,
    status               text         NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'confirmed', 'rescheduled', 'cancelled', 'completed', 'rejected')),
    status_reason        text,
    -- Set when the space was suspended after the booking (L3-G-290).
    flagged              boolean      NOT NULL DEFAULT false,
    flag_reason          text,
    process_instance_id  text,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (start_time < end_time),
    CHECK ((requester_kind IS NULL) = (requester_id IS NULL))
);
CREATE UNIQUE INDEX space_reservation_number ON campus.space_reservation (tenant_id, institution_id, number);
CREATE INDEX space_reservation_slot ON campus.space_reservation (tenant_id, node_id, reservation_date, status);
CREATE INDEX space_reservation_requester ON campus.space_reservation (tenant_id, requester_kind, requester_id);
CREATE TRIGGER space_reservation_no_delete BEFORE DELETE ON campus.space_reservation
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
