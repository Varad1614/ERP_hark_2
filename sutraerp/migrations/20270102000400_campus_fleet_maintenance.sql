-- ERP_EXTENSIONS 2D — fleet maintenance. Logic ported from HeyPuter/everything fleet
-- src/core/reminders.js, fuel.js, costs.js, dates.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- Vehicles gain a meter (odometer km by default, or an hour meter) and a tank size. Service
-- reminders repeat a task by time, by meter, or whichever comes first; renewals are dated
-- obligations (RC / registration, insurance, fitness certificate, PUC, permit, road tax) that
-- roll forward on an anchored repeat. Service entries complete reminders; fuel entries give
-- economy between full fills; expenses and meter readings feed the cost report.
ALTER TABLE campus.vehicle
    ADD COLUMN meter          numeric(12,1) CHECK (meter IS NULL OR meter >= 0),
    ADD COLUMN meter_unit     text          NOT NULL DEFAULT 'km' CHECK (meter_unit IN ('km', 'mi', 'hr')),
    ADD COLUMN fuel_capacity  numeric(8,2)  CHECK (fuel_capacity IS NULL OR fuel_capacity > 0);

CREATE TABLE campus.fleet_settings (
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    due_soon_days        integer       NOT NULL DEFAULT 14 CHECK (due_soon_days BETWEEN 0 AND 365),
    due_soon_meter       numeric(12,1) NOT NULL DEFAULT 500 CHECK (due_soon_meter >= 0),
    volume_unit          text          NOT NULL DEFAULT 'L' CHECK (volume_unit IN ('L', 'gal')),
    version              bigint        NOT NULL DEFAULT 1,
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

CREATE TABLE campus.vehicle_service_reminder (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    task                 text          NOT NULL CHECK (length(btrim(task)) BETWEEN 1 AND 200),
    interval_every       integer       CHECK (interval_every IS NULL OR interval_every BETWEEN 1 AND 1000),
    interval_unit        text          NOT NULL DEFAULT 'months' CHECK (interval_unit IN ('days', 'weeks', 'months', 'years')),
    meter_interval       numeric(12,1) CHECK (meter_interval IS NULL OR meter_interval > 0),
    due_soon_days        integer       CHECK (due_soon_days IS NULL OR due_soon_days BETWEEN 0 AND 365),
    due_soon_meter       numeric(12,1) CHECK (due_soon_meter IS NULL OR due_soon_meter >= 0),
    start_date           date          NOT NULL,
    start_meter          numeric(12,1),
    last_date            date,
    last_meter           numeric(12,1),
    next_date            date,
    next_meter           numeric(12,1),
    snoozed_until        date,
    history              jsonb         NOT NULL DEFAULT '[]'::jsonb,
    active               boolean       NOT NULL DEFAULT true,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (interval_every IS NOT NULL OR meter_interval IS NOT NULL OR next_date IS NOT NULL
           OR next_meter IS NOT NULL)
);
CREATE INDEX vehicle_service_reminder_vehicle_idx ON campus.vehicle_service_reminder (tenant_id, vehicle_id);

CREATE TABLE campus.vehicle_renewal (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    renewal_type         text          NOT NULL CHECK (renewal_type IN ('registration', 'insurance', 'fitness',
                                           'pollution', 'permit', 'road_tax', 'other')),
    label                text          CHECK (label IS NULL OR length(btrim(label)) BETWEEN 1 AND 200),
    due_date             date,
    due_soon_days        integer       CHECK (due_soon_days IS NULL OR due_soon_days BETWEEN 0 AND 365),
    repeat_every         integer       CHECK (repeat_every IS NULL OR repeat_every BETWEEN 1 AND 1000),
    repeat_unit          text          NOT NULL DEFAULT 'years' CHECK (repeat_unit IN ('days', 'weeks', 'months', 'years')),
    repeat_anchor        jsonb,
    snoozed_until        date,
    history              jsonb         NOT NULL DEFAULT '[]'::jsonb,
    done                 boolean       NOT NULL DEFAULT false,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX vehicle_renewal_vehicle_idx ON campus.vehicle_renewal (tenant_id, vehicle_id);

CREATE TABLE campus.vehicle_service_entry (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    serviced_on          date          NOT NULL,
    meter                numeric(12,1) CHECK (meter IS NULL OR meter >= 0),
    description          text          NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 500),
    vendor               text          CHECK (vendor IS NULL OR length(vendor) <= 200),
    amount               numeric(14,2) NOT NULL DEFAULT 0 CHECK (amount >= 0),
    reminder_ids         uuid[]        NOT NULL DEFAULT '{}',
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX vehicle_service_entry_vehicle_idx ON campus.vehicle_service_entry (tenant_id, vehicle_id, serviced_on);

CREATE TABLE campus.vehicle_fuel_entry (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    filled_on            date          NOT NULL,
    filled_time          text          CHECK (filled_time IS NULL OR filled_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    meter                numeric(12,1) CHECK (meter IS NULL OR meter >= 0),
    volume               numeric(10,3) NOT NULL CHECK (volume > 0),
    total                numeric(14,2) NOT NULL CHECK (total >= 0),
    partial              boolean       NOT NULL DEFAULT false,
    reset                boolean       NOT NULL DEFAULT false,
    kind                 text          NOT NULL DEFAULT 'fuel' CHECK (kind IN ('fuel', 'charge')),
    vendor               text          CHECK (vendor IS NULL OR length(vendor) <= 200),
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX vehicle_fuel_entry_vehicle_idx ON campus.vehicle_fuel_entry (tenant_id, vehicle_id, filled_on);

CREATE TABLE campus.vehicle_meter_reading (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    read_on              date          NOT NULL,
    value                numeric(12,1) NOT NULL CHECK (value >= 0),
    source               text          NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'fuel', 'service')),
    void                 boolean       NOT NULL DEFAULT false,
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX vehicle_meter_reading_vehicle_idx ON campus.vehicle_meter_reading (tenant_id, vehicle_id, read_on);

CREATE TABLE campus.vehicle_expense (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    vehicle_id           uuid          NOT NULL REFERENCES campus.vehicle (id),
    spent_on             date          NOT NULL,
    expense_type         text          NOT NULL CHECK (length(btrim(expense_type)) BETWEEN 1 AND 60),
    amount               numeric(14,2) NOT NULL CHECK (amount >= 0),
    note                 text          CHECK (note IS NULL OR length(note) <= 500),
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX vehicle_expense_vehicle_idx ON campus.vehicle_expense (tenant_id, vehicle_id, spent_on);

GRANT SELECT, INSERT, UPDATE, DELETE ON campus.fleet_settings, campus.vehicle_service_reminder,
    campus.vehicle_renewal TO sutra_campus;
GRANT SELECT, INSERT ON campus.vehicle_service_entry, campus.vehicle_fuel_entry,
    campus.vehicle_expense TO sutra_campus;
GRANT SELECT, INSERT, UPDATE ON campus.vehicle_meter_reading TO sutra_campus;
SELECT platform.apply_tenant_rls();
