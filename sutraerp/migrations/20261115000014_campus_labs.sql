-- Batch 8 slice 8A — laboratories (L2-G02; L3-G-050, 053, 057, 060): lab profile of a `lab`
-- space, readiness checklist, equipment status, practical sessions and batch allocation.

CREATE TABLE campus.lab (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    node_id              uuid         NOT NULL REFERENCES campus.infrastructure_node (id),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    capacity             integer      NOT NULL CHECK (capacity >= 1),
    status               text         NOT NULL DEFAULT 'not_ready'
        CHECK (status IN ('not_ready', 'ready', 'restricted', 'unavailable')),
    status_reason        text,
    -- [{item, done}] — the last readiness check.
    checklist            jsonb        NOT NULL DEFAULT '[]'::jsonb,
    readiness_notes      text,
    readiness_checked_at timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lab_node ON campus.lab (tenant_id, node_id);

CREATE TABLE campus.lab_equipment (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    lab_id           uuid         NOT NULL REFERENCES campus.lab (id),
    code             text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_.-]{0,31}$'),
    name             text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    status           text         NOT NULL DEFAULT 'operational'
        CHECK (status IN ('operational', 'restricted', 'under_maintenance', 'out_of_service')),
    status_reason    text,
    version          bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lab_equipment_code ON campus.lab_equipment (tenant_id, lab_id, code);

CREATE TABLE campus.practical_session (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    lab_id           uuid         NOT NULL REFERENCES campus.lab (id),
    title            text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    course_code      text,
    session_date     date         NOT NULL,
    start_time       time         NOT NULL,
    end_time         time         NOT NULL,
    capacity         integer      NOT NULL CHECK (capacity >= 1),
    status           text         NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'ready', 'in_progress', 'completed', 'postponed', 'cancelled')),
    status_reason    text,
    version          bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz  NOT NULL DEFAULT now(),
    updated_at       timestamptz  NOT NULL DEFAULT now(),
    CHECK (start_time < end_time)
);
CREATE INDEX practical_session_lab ON campus.practical_session (tenant_id, lab_id, session_date);

CREATE TABLE campus.lab_batch_allocation (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    session_id       uuid         NOT NULL REFERENCES campus.practical_session (id),
    person_kind      text         NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id        uuid         NOT NULL,
    batch_label      text,
    created_by       text,
    created_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lab_batch_allocation_person
    ON campus.lab_batch_allocation (tenant_id, session_id, person_kind, person_id);
