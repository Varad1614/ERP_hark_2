-- Slice 2B: institutional campuses (L2-B03; L3/L4-B-017 … B-022).
--
-- The campus is the *institutional* representation of a site (B03). Physical facts (buildings,
-- spaces, authoritative capacity, accessibility) belong to Domain G (G10) and are not stored
-- here; `declared_capacity` is the registration-time declaration only and is never changed by a
-- B03 modification.
--
-- Lifecycle (contract §4.2 `CampusStatus`): registered (L4 "Proposed") → pending_activation
-- (activation decision in progress) → active (L4 "Operational") ⇄ suspended (L4 "Temporarily
-- Closed"); active → decommissioned → archived (terminal, read-only).
CREATE TABLE institution.campus (
    id                       uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid        NOT NULL,
    institution_id           uuid        NOT NULL REFERENCES institution.institution(id),
    code                     text        NOT NULL,
    name                     text        NOT NULL,
    campus_type              text        NOT NULL CHECK (campus_type IN (
                                 'main', 'satellite', 'city', 'medical', 'engineering',
                                 'research', 'off_campus', 'online', 'other')),
    is_main                  boolean     NOT NULL DEFAULT false,
    address_line1            text,
    address_line2            text,
    city                     text,
    state                    text,
    postal_code              text,
    country                  text        NOT NULL DEFAULT 'IN' CHECK (country ~ '^[A-Z]{2}$'),
    latitude                 double precision CHECK (latitude BETWEEN -90 AND 90),
    longitude                double precision CHECK (longitude BETWEEN -180 AND 180),
    timezone                 text,
    established_on           date,
    declared_capacity        integer     CHECK (declared_capacity > 0),
    contact_email            text,
    contact_phone            text,
    description              text,
    working_hours            text,
    status                   text        NOT NULL DEFAULT 'registered' CHECK (status IN (
                                 'registered', 'pending_activation', 'active', 'suspended',
                                 'decommissioned', 'archived')),
    -- Governed decision awaiting a decision-maker (WorkflowPort AwaitDecision mode).
    pending_action           text        CHECK (pending_action IN ('activation', 'decommission')),
    pending_reason           text,
    pending_effective_on     date,
    process_instance_id      text,
    status_reason            text,
    decommission_effective_on date,
    status_changed_at        timestamptz NOT NULL DEFAULT now(),
    version                  bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at               timestamptz NOT NULL DEFAULT now(),
    updated_at               timestamptz NOT NULL DEFAULT now(),
    -- Codes are never reused, not even after archival (L4-B-022-BH-007).
    CONSTRAINT campus_code_unique UNIQUE (tenant_id, institution_id, code),
    CONSTRAINT campus_pending_activation_consistent CHECK (
        (status = 'pending_activation') = (pending_action IS NOT DISTINCT FROM 'activation'))
);

-- At most one main campus among the live campuses of an institution.
CREATE UNIQUE INDEX campus_one_main ON institution.campus (institution_id)
    WHERE is_main AND status NOT IN ('decommissioned', 'archived');
CREATE INDEX campus_institution_status ON institution.campus (tenant_id, institution_id, status);

-- Every accepted change of a campus (registration, modification, lifecycle transition) keeps a
-- full snapshot: prior metadata and historical interpretation stay resolvable (L4-B-019-BH-007).
CREATE TABLE institution.campus_revision (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL REFERENCES institution.institution(id),
    campus_id       uuid        NOT NULL REFERENCES institution.campus(id),
    revision        bigint      NOT NULL,
    kind            text        NOT NULL CHECK (kind IN ('registered', 'modified', 'status')),
    changed_fields  text[]      NOT NULL DEFAULT '{}',
    from_status     text,
    to_status       text,
    reason          text,
    actor           text,
    snapshot        jsonb       NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT campus_revision_unique UNIQUE (campus_id, revision)
);
